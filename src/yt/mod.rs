//! The YouTube layer — everything that talks to the `yt-dlp` CLI.
//!
//! One-way dependency, like `api/`: spawns the yt-dlp process and hands plain
//! data back to the app over its return values; nothing here touches `App` or
//! the render tree.
//!
//! Phase 1 of the Spotify → YouTube port. Read-only and self-contained; every
//! parser is tested offline on canned `-J` JSON, so developing against it needs
//! no YouTube account, no cookies and no network. The live smoke tests that do
//! need a network are marked `#[ignore]`, matching the project convention.

//! The phase-2 engine and phase-3 api layer are the callers; fields stay public
//! so the bin-side expander can repackage rows. The allow drops those few items
//! no caller has reached yet.
#![allow(dead_code)]

use crate::config;
use crate::liblog::liblog;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Semaphore, SemaphorePermit};

/// A cap on concurrent yt-dlp children (F17): every app surface (engine
/// per-track resolve, search thread, drill-in thread, radio chains) funnels a
/// fresh `Command` through `yt_stdout`, and each child is a new ~50–80MB
/// Python with a ~300–500ms startup. Two permits let two surfaces overlap
/// without stacking a herd. Contention beyond the budget FAILS OPEN (see
/// [`wait_for_permit`]) — never a spurious failure, which the engine would
/// read as a dropped stream and burn a recovery retry on.
///
/// `tokio::sync::Semaphore` rather than `std`: no `std::sync::Semaphore`
/// exists on the toolchain this crate builds with (rustc 1.97.1); tokio's is
/// already a dependency of the `streaming` feature that gates this module.
static YTDLP_PERMIT: Semaphore = Semaphore::const_new(2);

/// How long a single yt-dlp socket can stall before the process gives up. A
/// hung network must never wedge a worker thread forever; the app's own
/// radio/playback deadlines sit above this (see `RADIO_TIMEOUT_SECS`).
const SOCKET_TIMEOUT_SECS: u32 = 10;
/// Extra headroom added to [`SOCKET_TIMEOUT_SECS`] when sizing the process
/// deadline: yt-dlp's own retry/internal handling can outlive one socket
/// timeout by a few seconds, and a stalled worker must still give up.
const DEADLINE_MARGIN_SECS: u32 = 5;

/// One playable YouTube video: a search result, a playlist entry, or a resolved
/// single video. Rows that cannot be played (no video id) are dropped by the
/// parsers rather than surfaced as empty rows.
pub struct YtVideo {
    /// Canonical internal uri: `yt:video:<id>` — the id itself is derivable
    /// (`track_id_from_uri`), so rows carry only the uri.
    pub uri: String,
    pub title: String,
    /// Best-known performer name: `artist` when MusicBrainz-tagged, else the
    /// channel/uploader name. Empty on flat playlist rows, which are title-only.
    pub artist: String,
    /// Absent on all but a few MusicBrainz-tagged videos.
    pub album: Option<String>,
    /// Missing on some flat playlist rows; `None` rows can't drive the progress
    /// bar or the lrclib duration key until enriched.
    pub duration_ms: Option<u32>,
    /// Best available thumbnail (last, largest in the array), when present.
    pub thumbnail: Option<String>,
}

/// A playable stream: the direct audio URL plus the metadata playback needs.
pub struct StreamInfo {
    pub url: String,
    pub video: YtVideo,
}

/// `ytsearchN:` — N YouTube video results for a query. Fast (one request):
/// flat mode uses only the search API's own metadata, no per-video resolution.
pub fn search(query: &str, limit: usize) -> Vec<YtVideo> {
    let limit = limit.max(1);
    let Some(root) = yt_json(
        &["--flat-playlist", &format!("ytsearch{limit}:{query}")],
        None,
    ) else {
        return Vec::new();
    };
    entries(&root)
}

/// Full metadata for one video, from a bare video id, a `yt:video:` uri, or a
/// `youtube.com/watch?v=` URL. `--no-playlist` keeps a video that happens to be
/// inside a mix from expanding into its whole playlist.
pub fn video_meta(url_or_id: &str) -> Option<YtVideo> {
    let url = crate::util::video_url(url_or_id)?;
    let root = yt_json(&["--no-playlist", &url], None)?;
    video_from(&root)
}

/// The direct audio stream URL for one video, plus the metadata resolution
/// needed to play it (title, duration, thumbnail, artist) — one `-J` call
/// carries both (the `-f`/extractor-args policy below, then [`pick_url`]).
pub fn resolve(url_or_id: &str) -> Option<StreamInfo> {
    let url = crate::util::video_url(url_or_id)?;
    let configured = config::get().audio_format.clone();
    // `bestaudio/best` under the android client means `best` (the muxed
    // stream) — but a user who set `bestaudio` without a fallback must not
    // hard-fail resolution, so always carry the `/best` tail.
    let format = if configured.contains('/') {
        configured.clone()
    } else {
        format!("{configured}/best")
    };
    let root = yt_json(
        &[
            "--no-playlist",
            "-f",
            &format,
            &url,
            "--extractor-args",
            &format!("youtube:player_client={STREAM_PLAYER_CLIENT}"),
        ],
        None,
    )?;
    let video = video_from(&root)?;
    let stream = pick_url(&root, &configured)?;
    Some(StreamInfo {
        url: stream.to_string(),
        video,
    })
}

/// How long a radio request may take before the app gives up. The station
/// fetch is capped to one inner-page (~4s), so this only covers the rare
/// fallback chain (mix-id variants + a search-built station) and genuine
/// network stalls — 12s was shorter than an un-capped mix's pagination alone
/// (20s+), which made healthy-but-large mixes read as endpoint timeouts.
pub const RADIO_TIMEOUT_SECS: u64 = 20;

/// How many entries the radio fetch asks for — the engine's station cap,
/// clipped to one or two inner-pages at most: un-capped, yt-dlp paginates the
/// whole mix — hundreds of rows across 15+ sequential API pages, 20s+ even on
/// a healthy network (measured 2026-08-16) — and the app's radio deadline
/// fires for what is really a too-greedy fetch. `--playlist-end` at this cap
/// costs ~4s per mix. The clip below is `min(station cap, 40)` — it can never
/// paginate past the engine's station slice, so no pin is needed.
const RADIO_FETCH_LIMIT: usize = if crate::engine::RADIO_LIMIT < 40 {
    crate::engine::RADIO_LIMIT
} else {
    40
};

/// Candidate mix URLs for a seed video, in preference order: the canonical
/// `RD<id>` mix, then its autoplay-video variant `RDAMVM<id>`. The id cannot
/// be read from the watch response — neither yt-dlp's `-J` output nor the
/// watch HTML carry the current video's mix id (verified 2026-08-16); the
/// convention is deterministic (`RD` + video id, confirmed by the page's own
/// `start_radio` command URLs), so these are the only two shapes worth trying.
fn radio_candidates(id: &str) -> Vec<String> {
    vec![
        format!("https://www.youtube.com/watch?v={id}&list=RD{id}"),
        format!("https://www.youtube.com/watch?v={id}&list=RDAMVM{id}"),
    ]
}

/// A radio station for a seed video id: the first mix candidate that serves
/// rows, else a search-built pseudo-radio. YouTube has no mix for fresh or
/// obscure uploads — `--flat-playlist` on their `RD<id>` URL degrades to a
/// single-video dump (zero entries), which used to read as a hard radio
/// failure. The pseudo-radio resolves the seed for its title, searches that
/// flat, and drops the seed itself from the results.
pub fn radio_entries(id: &str, cancel: Arc<AtomicBool>) -> Vec<YtVideo> {
    let limit = RADIO_FETCH_LIMIT.to_string();
    for url in radio_candidates(id) {
        // F13: check between chain steps — a cancelled request must not start
        // the next candidate's child. An empty result is the cancelled
        // request's signature; the caller (spawn_radio's timeout) has already
        // reported the failure and only the drain resets need this to arrive.
        if cancel.load(Ordering::Relaxed) {
            return Vec::new();
        }
        if let Some(root) = yt_json(
            &["--flat-playlist", "--playlist-end", &limit, &url],
            Some(cancel.clone()),
        ) {
            let rows = entries(&root);
            if !rows.is_empty() {
                return rows;
            }
            liblog(format!("yt: {url} served no playlist rows"));
        }
    }
    // F13: the pseudo-radio leg (two more children) is cancelled too.
    if cancel.load(Ordering::Relaxed) {
        return Vec::new();
    }
    pseudo_radio(id, cancel)
}

/// Last-resort station: search the seed's own title when no mix exists.
///
/// Everything here rides the search API, NOT the player endpoint: the videos
/// that lack a mix are exactly the fresh/obscure ones whose watch pages get
/// player-level bot-gated ("Sign in to confirm"), while `ytsearchN:` stays
/// open — so the seed's metadata is fetched by searching the id itself, and
/// the station is a flat search on that title.
fn pseudo_radio(id: &str, cancel: Arc<AtomicBool>) -> Vec<YtVideo> {
    let Some(seed) = yt_json(
        &["--flat-playlist", &format!("ytsearch1:{id}")],
        Some(cancel.clone()),
    )
    .and_then(|root| entries(&root).into_iter().next()) else {
        return Vec::new();
    };
    // F13: check between the two legs — the search-backed station is two
    // children; a cancelled request skips the second.
    if cancel.load(Ordering::Relaxed) {
        return Vec::new();
    }
    let query = pseudo_radio_query(&seed.title, &seed.artist);
    if query.is_empty() {
        return Vec::new();
    }
    let Some(root) = yt_json(
        &[
            "--flat-playlist",
            &format!("ytsearch{}:{query}", RADIO_FETCH_LIMIT),
        ],
        Some(cancel),
    ) else {
        return Vec::new();
    };
    entries(&root)
        .into_iter()
        .filter(|v| v.uri != format!("yt:video:{id}"))
        .collect()
}

/// The search query a pseudo-radio station is built from: `artist - title`
/// when the artist is known, else the bare title. Pure, so the fallback's
/// query shape is pinned by a test rather than drifting with the resolvers.
fn pseudo_radio_query(title: &str, artist: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        return String::new();
    }
    let artist = artist.trim();
    if artist.is_empty() {
        title.to_string()
    } else {
        format!("{artist} - {title}")
    }
}

/// The entries of a playlist, mix or channel tab, flat-extracted: each row is
/// the playlist API's own metadata (title, id, duration when present) without
/// per-video resolution — the cheap `-J --flat-playlist` used for browsing.
pub fn playlist_entries(url: &str) -> Vec<YtVideo> {
    let Some(root) = yt_json(&["--flat-playlist", url], None) else {
        return Vec::new();
    };
    // A bare `watch?v=` URL with no `list=` yields a single-video dump with no
    // `entries`, which would otherwise read as an empty playlist. Say so
    // rather than vanishing silently (the caller can't tell the difference).
    if root["entries"].is_null() {
        liblog(format!("yt: {url} is not a playlist dump; no entries"));
        return Vec::new();
    }
    entries(&root)
}

/// The contents of a context kind. One table owns the kind → resource
/// mapping every layer consumes: playback expansion and the drill-in view
/// used to re-implement it separately and drifted on the channel URL shape.
/// `video` is absent on purpose — a single track's contents are itself, and
/// both callers already pass it through.
pub fn resolve_kind(kind: &str, id: &str, limit: usize) -> Vec<YtVideo> {
    match kind {
        "playlist" => playlist_entries(&crate::util::playlist_uri(id)),
        "channel" => playlist_entries(&crate::util::channel_videos_url(id)),
        "album" => search(id, limit),
        _ => Vec::new(),
    }
}

/// `Videos` rows from any playlist-shaped `-J` dump (`entries:` array).
fn entries(root: &serde_json::Value) -> Vec<YtVideo> {
    root["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(video_from)
        .collect()
}

/// One `entries[]` row -> `YtVideo`. `None` drops rows with no video id — the
/// flat-playlist equivalent of `api/library.rs`'s region-locked-row skip.
fn video_from(v: &serde_json::Value) -> Option<YtVideo> {
    let id = v["id"].as_str()?;
    let title = v["title"].as_str().unwrap_or("").to_string();
    let artist = v["artist"]
        .as_str()
        .or_else(|| v["channel"].as_str())
        .or_else(|| v["uploader"].as_str())
        .unwrap_or("")
        .to_string();
    let album = v["album"].as_str().map(String::from);
    let duration_ms = v["duration"]
        .as_u64()
        // checked_mul: `s * 1000` would wrap/panic on a hostile `-J` dump
        // (u64::MAX × 1000 overflows before the u32 try_from can guard it).
        .and_then(|s| s.checked_mul(1000))
        .and_then(|ms| u32::try_from(ms).ok());
    let thumbnail = largest_thumbnail(v);
    let uri = format!("yt:video:{id}");
    Some(YtVideo {
        uri,
        title,
        artist,
        album,
        duration_ms,
        thumbnail,
    })
}

/// The last thumbnail in the array — yt-dlp orders `thumbnails` small→large, so
/// the last is the biggest actual frame. Falls back to the bare `thumbnail`.
fn largest_thumbnail(v: &serde_json::Value) -> Option<String> {
    v["thumbnails"]
        .as_array()
        .and_then(|a| a.last())
        .and_then(|t| t["url"].as_str())
        .or_else(|| v["thumbnail"].as_str())
        .map(String::from)
}

/// Wait up to `deadline` for one of `p`'s permits, polling every 50ms.
/// `None` means the budget is exhausted: the caller MUST fail open (spawn
/// anyway and block for a permit) — a permit-shaped `None` must never surface
/// as a request failure, because `yt_stdout`'s `None` is a dropped stream to
/// the engine. In production `None` only appears under pathological
/// contention, where the fail-open fallback degrades to today's unbounded
/// behavior.
fn wait_for_permit(p: &Semaphore, deadline: Instant) -> Option<SemaphorePermit<'_>> {
    loop {
        if let Ok(permit) = p.try_acquire() {
            return Some(permit);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Run `yt-dlp … -J` and parse its dumped JSON.
fn yt_json(extra: &[&str], cancel: Option<Arc<AtomicBool>>) -> Option<serde_json::Value> {
    yt_stdout(&["-J"], extra, cancel).and_then(|s| serde_json::from_str(&s).ok())
}

/// The stream leg is resolved with the `android` player client. On this box
/// (verified 2026-08-16, bead Myx-jqp) URLs from the default/web/tv clients
/// stall at the transport level — ffmpeg connects and receives 0 bytes in
/// 15 s, while the `android` client's URL flows instantly (~180 ms to first
/// PCM, 3/3 runs vs 0/9 for the others). A stalled URL starves the engine's
/// prebuffer → silent playback, ALSA underrun errors, frozen position and
/// visualizer, and watchdog rebuild loops.
///
/// Trade-off: the android client exposes no audio-only itags — `-f bestaudio`
/// hard-fails under it, so the `/best` fallback below always lands on the
/// muxed (360p video + audio) stream, wasting bandwidth the engine never
/// decodes. That cost is deliberate: an unthrottled stream is worth 3× the
/// bytes; a user-set `audio_format` without a fallback is tolerated by
/// appending `/best` rather than letting resolution fail.
const STREAM_PLAYER_CLIENT: &str = "android";

/// Pull the direct stream URL out of a `-J` info dump — the same pick the old
/// `-g -f <configured>/best` leg made, now from the dump's own data:
/// 1. a `formats[]` entry whose `format_id` equals a bare `configured` (a
///    user's `audio_format = "251"` must select that exact itag);
/// 2. otherwise the *last playable* entry — the android dump appends
///    storyboard entries (`sb*`, both codecs "none") after the real stream,
///    so the naive "last entry" would hand back a storyboard URL;
/// 3. finally the info dict's own top-level `url` — which with `-f` active is
///    exactly the format `-g` would have printed.
fn pick_url<'a>(root: &'a serde_json::Value, configured: &str) -> Option<&'a str> {
    if let Some(entries) = root["formats"].as_array() {
        if !configured.contains('/') {
            if let Some(f) = entries
                .iter()
                .find(|f| f["format_id"].as_str() == Some(configured))
                .and_then(|f| f["url"].as_str())
            {
                return Some(f);
            }
        }
        if let Some(f) = entries.iter().rev().find(|f| {
            let playable = f["vcodec"].as_str().is_some_and(|v| v != "none")
                || f["acodec"].as_str().is_some_and(|a| a != "none");
            playable && f["url"].as_str().is_some()
        }) {
            return f["url"].as_str();
        }
    }
    root["url"].as_str()
}

/// Run the configured yt-dlp binary with `base + extra` args and capture
/// stdout; `None` on launch failure, non-zero exit, or invalid UTF-8. Errors
/// are logged, not propagated — the api layer's Option/empty convention.
///
/// Two bounds keep a yt-dlp child from ever wedging a worker thread:
/// `--retries 1` cuts yt-dlp's own default retry chain (which can sleep for
/// minutes on a 429), and the child is killed outright once it outlives
/// `SOCKET_TIMEOUT_SECS` + a small margin (a stalled socket must not hold a
/// blocking thread hostage while the app's own deadline has given up on it).
///
/// The pipes MUST be drained while the child runs: a `-J` dump is ~600 KB and
/// yt-dlp blocks on a full 64 KB pipe unless something reads it, so a plain
/// poll-and-kill loop would deadlock until the deadline kills the child.
/// Reader threads therefore own the drains; the caller thread owns the child
/// (for `try_wait`/`kill`, which need `&mut`) and joins the readers once the
/// process is gone. Killing guarantees EOF on both pipes, so the joins always
/// return.
fn yt_stdout(base: &[&str], extra: &[&str], cancel: Option<Arc<AtomicBool>>) -> Option<String> {
    let bin = config::get().ytdlp_path.clone();
    yt_stdout_with_bin(&bin, base, extra, cancel)
}

/// The [`yt_stdout`] core with the binary injected — the config read stays at
/// the public boundary so tests can point the seam at a fake binary and run
/// the cancellability (F13) and concurrency-cap (F17) paths fully offline.
fn yt_stdout_with_bin(
    bin: &str,
    base: &[&str],
    extra: &[&str],
    cancel: Option<Arc<AtomicBool>>,
) -> Option<String> {
    // F17: the RAII permit is held across the child's WHOLE life — spawn
    // through both drain joins — and drops on every early-return path below.
    // The bounded wait shares the child's own deadline; when the budget is
    // exhausted the wait fails OPEN (spawn anyway once a permit frees),
    // degrading to today's behavior instead of manufacturing a resolve
    // failure the engine would treat as a dropped stream.
    let deadline =
        Instant::now() + Duration::from_secs((SOCKET_TIMEOUT_SECS + DEADLINE_MARGIN_SECS) as u64);
    let _permit = match wait_for_permit(&YTDLP_PERMIT, deadline) {
        Some(permit) => permit,
        None => {
            liblog("yt: yt-dlp budget exhausted — waiting beyond deadline (fail-open)");
            loop {
                if let Ok(permit) = YTDLP_PERMIT.try_acquire() {
                    break permit;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    let mut child = std::process::Command::new(bin)
        .args([
            "--no-warnings",
            "--socket-timeout",
            &SOCKET_TIMEOUT_SECS.to_string(),
            "--retries",
            "1",
        ])
        .args(base)
        // A TUI's stdin must never leak into the CLI child: with tuna-tui run under a
        // pipe (tests, streaming, probes) yt-dlp can stall on an inherited,
        // never-EOF stdin instead of doing its job. ffmpeg gets the same
        // treatment (`spawn_ffmpeg`).
        .stdin(std::process::Stdio::null())
        .args(extra)
        // Optional session cookies (`--cookies`): without them private
        // playlists / history are inaccessible and traffic is bot-checked.
        .args(match &config::get().cookies_file {
            Some(path) => vec!["--cookies", path.as_str()],
            None => vec![],
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || drain(&mut stdout));
    let stderr_reader = std::thread::spawn(move || drain(&mut stderr));
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(_) => return None,
        }
        // F13: a per-request cancel (spawn_radio's timeout Err branch sets it)
        // kills the child on the next 50ms poll instead of letting a radio
        // chain keep spawning Python for ~40s after the UI has given up.
        if cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
            liblog(format!("yt: {bin} killed on cancellation"));
            let _ = child.kill();
            break child.wait().ok()?;
        }
        if Instant::now() >= deadline {
            liblog(format!("yt: {bin} killed after exceeding its deadline"));
            let _ = child.kill();
            break child.wait().ok()?;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    // Both pipes EOF once the process is gone (kill included), so these joins
    // cannot hang.
    let out = stdout_reader.join().ok()?;
    let err = stderr_reader.join().ok()?;
    if !status.success() {
        let code = status.code().unwrap_or(-1);
        let tail = String::from_utf8_lossy(&err)
            .lines()
            .next_back()
            .unwrap_or("")
            .to_string();
        liblog(format!(
            "yt: {bin} exited {code}: {tail} (args {:?})",
            extra.iter().take(3).collect::<Vec<_>>()
        ));
        return None;
    }
    String::from_utf8(out).ok()
}

/// Read one pipe to EOF into a vector. Runs on the reader threads; EOF is
/// guaranteed by the process exiting (or being killed).
fn drain<R: std::io::Read + Send>(pipe: &mut Option<R>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(pipe) = pipe.as_mut() {
        let _ = pipe.read_to_end(&mut buf);
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `ytsearch3:bohemian rhapsody queen` `-J --flat-playlist` dump
    /// (trimmed to two entries, 2026-08-16, yt-dlp 2026.07.04).
    const SEARCH_JSON: &str = r#"{
        "_type": "playlist", "id": "search", "title": "bohemian rhapsody queen",
        "entries": [
            {
                "_type": "url", "availability": "public", "channel": "Queen Official",
                "channel_id": "UCiMhD4jzUqG-IgPzUmmytRQ", "channel_url": "https://www.youtube.com/channel/UCiMhD4jzUqG-IgPzUmmytRQ",
                "description": "…", "duration": 360, "id": "fJ9rUzIMcZQ",
                "ie_key": "Youtube", "live_status": "not_live", "release_timestamp": 1183641930,
                "thumbnails": [{"url": "https://i.ytimg.com/vi/fJ9rUzIMcZQ/hqdefault.jpg"}],
                "title": "Queen – Bohemian Rhapsody (Official Video Remastered)",
                "uploader": "Queen Official", "url": "https://www.youtube.com/watch?v=fJ9rUzIMcZQ",
                "view_count": 2026071363
            },
            {
                "_type": "url", "channel": "Queen Official", "channel_id": "UCiMhD4jzUqG-IgPzUmmytRQ",
                "duration": 255, "id": "JofwEB9g1K8",
                "thumbnails": [{"url": "https://i.ytimg.com/vi/JofwEB9g1K8/hqdefault.jpg"}],
                "title": "Queen – Bohemian Rhapsody (1987 Live At Wembley)"
            }
        ]
    }"#;

    /// A real single-video `-J` dump (Rick Astley, 2026-08-16), trimmed to the
    /// fields the parser reads plus a realistic thumbnail array ordering.
    const VIDEO_JSON: &str = r#"{
        "id": "dQw4w9WgXcQ", "title": "Rick Astley - Never Gonna Give You Up (Official Video)",
        "duration": 213, "channel": "Rick Astley", "uploader": "Rick Astley",
        "webpage_url": "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        "thumbnails": [
            {"url": "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg", "width": 480, "height": 360},
            {"url": "https://i.ytimg.com/vi_webp/dQw4w9WgXcQ/maxresdefault.webp", "width": 1280, "height": 720}
        ],
        "artist": null, "album": null
    }"#;

    /// A real flat playlist dump, trimmed to two entries (YouTube Radio Mix).
    /// Flat archive rows are title/id/duration-only: no channel, no thumbnails.
    const PLAYLIST_JSON: &str = r#"{
        "_type": "playlist", "id": "RDdQw4w9WgXcQ", "title": "Mix - Rick Astley - Never Gonna Give You Up",
        "entries": [
            {"_type": "url", "id": "dQw4w9WgXcQ", "title": "Rick Astley - Never Gonna Give You Up (Official Video)", "url": "https://www.youtube.com/watch?v=dQw4w9WgXcQ", "duration": 213},
            {"_type": "url", "id": "l9nh1l8ZIJQ", "title": "Survivor - Eye Of The Tiger (Official HD Video)", "url": "https://www.youtube.com/watch?v=l9nh1l8ZIJQ", "duration": 246}
        ]
    }"#;

    /// A real `-J` dump from the android player client (captured 2026-08-17,
    /// same shape `resolve` consumes), trimmed and with the signed stream URLs
    /// replaced by placeholders. The structure is the point: the storyboard
    /// entry (`sb*`, both codecs "none") comes *after* the only playable mux,
    /// so a naive "last formats entry" pick would hand back a storyboard URL.
    const ANDROID_JSON: &str = r#"{
        "_type": "video", "id": "dQw4w9WgXcQ",
        "title": "Rick Astley - Never Gonna Give You Up (Official Video) (4K Remaster)",
        "duration": 213, "channel": "Rick Astley",
        "thumbnails": [
            {"url": "https://i.ytimg.com/vi_webp/dQw4w9WgXcQ/maxresdefault.webp", "preference": 0, "id": "37"}
        ],
        "url": "https://googlevideo.example/videoplayback?itag=18",
        "formats": [
            {"format_id": "sb1", "url": "https://i.ytimg.com/sb/dQw4w9WgXcQ/storyboard3_L1", "vcodec": "none", "acodec": "none"},
            {"format_id": "18", "url": "https://googlevideo.example/videoplayback?itag=18", "vcodec": "avc1.42001E", "acodec": "mp4a.40.2"}
        ]
    }"#;

    #[test]
    fn search_entries_parse_and_inherit_channel_as_artist() {
        let root: serde_json::Value = serde_json::from_str(SEARCH_JSON).unwrap();
        let vids = entries(&root);
        assert_eq!(vids.len(), 2);
        assert_eq!(vids[0].uri, "yt:video:fJ9rUzIMcZQ");
        assert_eq!(vids[0].uri, "yt:video:fJ9rUzIMcZQ");
        assert_eq!(
            vids[0].title,
            "Queen – Bohemian Rhapsody (Official Video Remastered)"
        );
        // Flat search rows have no `artist` tag — fall back to channel/uploader.
        assert_eq!(vids[0].artist, "Queen Official");
        assert_eq!(vids[0].duration_ms, Some(360_000));
        assert_eq!(
            vids[0].thumbnail.as_deref(),
            Some("https://i.ytimg.com/vi/fJ9rUzIMcZQ/hqdefault.jpg")
        );
        // Second entry's uploader field is absent — channel fallback still lands.
        assert_eq!(vids[1].artist, "Queen Official");
    }

    #[test]
    fn rows_without_an_id_are_dropped() {
        let root: serde_json::Value = serde_json::from_str(
            r#"{"entries": [
                {"id": "abc123", "title": "ok"},
                {"title": "no id — malformed row"},
                {"id": 42, "title": "non-string id"}
            ]}"#,
        )
        .unwrap();
        let vids = entries(&root);
        assert_eq!(vids.len(), 1);
        assert_eq!(vids[0].uri, "yt:video:abc123");
    }

    #[test]
    fn single_video_parses_full_meta_and_prefers_largest_thumbnail() {
        let root: serde_json::Value = serde_json::from_str(VIDEO_JSON).unwrap();
        let v = video_from(&root).expect("video row");
        assert_eq!(v.uri, "yt:video:dQw4w9WgXcQ");
        assert_eq!(v.artist, "Rick Astley");
        assert_eq!(v.duration_ms, Some(213_000));
        assert_eq!(v.album, None);
        // Thumbnails are ordered small→large; the last one wins.
        assert_eq!(
            v.thumbnail.as_deref(),
            Some("https://i.ytimg.com/vi_webp/dQw4w9WgXcQ/maxresdefault.webp")
        );
    }

    #[test]
    fn flat_playlist_rows_are_title_only() {
        let root: serde_json::Value = serde_json::from_str(PLAYLIST_JSON).unwrap();
        let vids = entries(&root);
        assert_eq!(vids.len(), 2);
        // Flat archive rows carry no channel — artist is empty, not fabricated.
        assert_eq!(vids[0].artist, "");
        assert_eq!(vids[0].duration_ms, Some(213_000));
        assert_eq!(vids[1].uri, "yt:video:l9nh1l8ZIJQ");
        assert_eq!(vids[1].thumbnail, None);
    }

    #[test]
    fn watch_url_normalizes_id_uri_and_url() {
        // The builder moved to util — its contract is owned there now.
        assert_eq!(
            crate::util::video_url("yt:video:dQw4w9WgXcQ").as_deref(),
            Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ")
        );
        assert_eq!(
            crate::util::video_url("dQw4w9WgXcQ").as_deref(),
            Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ")
        );
        assert_eq!(
            crate::util::video_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ").as_deref(),
            Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ")
        );
        assert_eq!(crate::util::video_url("yt:playlist:PLabc"), None); // not a video
        assert_eq!(crate::util::video_url(""), None);
    }

    #[test]
    fn pick_url_skips_storyboards_and_takes_the_last_playable() {
        let root: serde_json::Value = serde_json::from_str(ANDROID_JSON).unwrap();
        // Default `bestaudio/best` (has a '/') → the last playable entry, not
        // the trailing sb1 storyboard.
        let url = pick_url(&root, "bestaudio/best").expect("mux url");
        assert!(url.contains("itag=18"));
        assert!(!url.contains("storyboard"));
    }

    #[test]
    fn pick_url_prefers_a_bare_format_id_match() {
        let root: serde_json::Value = serde_json::from_str(ANDROID_JSON).unwrap();
        let url = pick_url(&root, "18").expect("exact itag");
        assert!(url.contains("itag=18"));
        // A configured id with no formats[] match falls through to the last
        // playable entry too — never to the storyboard.
        let url = pick_url(&root, "251").expect("fallback mux");
        assert!(url.contains("itag=18"));
    }

    #[test]
    fn pick_url_falls_back_to_the_top_level_url() {
        let root: serde_json::Value =
            serde_json::from_str(r#"{"id": "x", "url": "https://googlevideo.example/plain"}"#)
                .unwrap();
        assert_eq!(
            pick_url(&root, "bestaudio/best"),
            Some("https://googlevideo.example/plain")
        );
        // No url anywhere — nothing to play.
        let root: serde_json::Value = serde_json::from_str(r#"{"id": "x"}"#).unwrap();
        assert_eq!(pick_url(&root, "bestaudio/best"), None);
    }

    #[test]
    fn android_dump_feeds_video_from_the_same_metadata_leg() {
        let root: serde_json::Value = serde_json::from_str(ANDROID_JSON).unwrap();
        let v = video_from(&root).expect("video row");
        assert_eq!(v.uri, "yt:video:dQw4w9WgXcQ");
        assert_eq!(v.artist, "Rick Astley");
        assert_eq!(v.duration_ms, Some(213_000));
    }

    #[test]
    fn duration_caps_without_wrapping() {
        let root: serde_json::Value =
            serde_json::from_str(r#"{"id": "x", "title": "t", "duration": 4294968}"#).unwrap();
        // 4294968s * 1000 = 4294968000 > u32::MAX; the row keeps its id but
        // loses the duration rather than wrapping.
        let v = video_from(&root).unwrap();
        assert_eq!(v.duration_ms, None);
        assert_eq!(v.uri, "yt:video:x");
    }

    #[test]
    fn hostile_duration_does_not_overflow() {
        // u64::MAX × 1000 would panic in debug / wrap in release if the
        // multiply ran unchecked — the parser must degrade to None instead.
        let root: serde_json::Value =
            serde_json::from_str(r#"{"id": "y", "title": "t", "duration": 18446744073709551615}"#)
                .unwrap();
        let v = video_from(&root).unwrap();
        assert_eq!(v.duration_ms, None);
        assert_eq!(v.uri, "yt:video:y");
    }

    #[test]
    fn radio_candidates_prefer_rd_then_rdamvm() {
        let c = radio_candidates("dQw4w9WgXcQ");
        assert_eq!(c.len(), 2);
        assert_eq!(
            c[0],
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=RDdQw4w9WgXcQ"
        );
        assert_eq!(
            c[1],
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=RDAMVMdQw4w9WgXcQ"
        );
    }

    #[test]
    fn video_shaped_dumps_read_as_empty() {
        // The degrade path: fresh/obscure videos have no mix, and their
        // `-J --flat-playlist` dump is a single-video object with zero
        // entries. The radio chain must read that as "no rows" and move on to
        // the next source instead of treating it as a station.
        let root: serde_json::Value = serde_json::from_str(
            r#"{"_type": "video", "id": "dQw4w9WgXcQ", "title": "t", "duration": 213}"#,
        )
        .unwrap();
        assert!(entries(&root).is_empty());
    }

    #[test]
    fn pseudo_radio_query_prefers_artist_title_over_bare_title() {
        assert_eq!(
            pseudo_radio_query("Bohemian Rhapsody (Official Video)", "Queen Official"),
            "Queen Official - Bohemian Rhapsody (Official Video)"
        );
        assert_eq!(pseudo_radio_query("a title", ""), "a title");
        assert_eq!(pseudo_radio_query("   ", ""), "");
    }

    /// Live smoke test: needs yt-dlp + network. Run with `--ignored`.
    #[test]
    #[ignore]
    fn live_search_roundtrip() {
        let vids = search("bohemian rhapsody queen", 3);
        assert!(!vids.is_empty(), "expected at least one video");
        assert!(vids.iter().all(|v| v.uri.starts_with("yt:video:")));
        assert!(vids.iter().any(|v| !v.artist.is_empty()));
    }

    /// Live smoke test: needs yt-dlp + network. Run with `--ignored`.
    #[test]
    #[ignore]
    fn live_resolve_roundtrip() {
        let info = resolve("dQw4w9WgXcQ").expect("resolvable");
        assert!(info.url.starts_with("http"));
        assert_eq!(info.video.duration_ms.unwrap_or(0), 213_000);
        assert_eq!(info.video.artist, "Rick Astley");
    }

    /// Write an executable fake yt-dlp into `temp_dir()` (tests may write
    /// temp files — the httpcache scratch pattern) and return its path.
    /// `body` is the shell script the fake runs; `exec`-style bodies keep the
    /// child's pipes from being held by grandchildren after a kill.
    fn fake_bin(tag: &str, body: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("tuna-yt-dlp-fake-{tag}-{}.sh", std::process::id()));
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    /// F13: a cancelled `yt_stdout` call kills its child on the next 50ms
    /// poll — not at the 15s deadline — and returns `None`; without a cancel
    /// the same child runs into the deadline and is killed there.
    #[test]
    fn yt_stdout_cancel_kills_a_slow_child() {
        let path = fake_bin("sleep", "exec sleep 30");
        let bin = path.to_string_lossy().into_owned();

        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_t = cancel.clone();
        let bin_t = bin.clone();
        let t0 = Instant::now();
        let handle =
            std::thread::spawn(move || yt_stdout_with_bin(&bin_t, &["-J"], &[], Some(cancel_t)));
        std::thread::sleep(Duration::from_millis(200));
        cancel.store(true, Ordering::Relaxed);
        assert!(handle.join().expect("worker thread").is_none());
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "cancel must kill the child fast, took {:?}",
            t0.elapsed()
        );

        let t1 = Instant::now();
        assert!(yt_stdout_with_bin(&bin, &["-J"], &[], None).is_none());
        assert!(
            t1.elapsed() >= Duration::from_secs(10),
            "an uncancelled child must hit the 15s deadline, returned after {:?}",
            t1.elapsed()
        );

        let _ = std::fs::remove_file(&path);
    }

    /// F17: the bounded wait gives up (`None`) when the budget is exhausted —
    /// the call site fails open, it never becomes a failure — and acquires
    /// instantly when a permit is free. A local semaphore keeps the global
    /// `YTDLP_PERMIT` untouched (tests run in parallel).
    #[test]
    fn wait_for_permit_bounds_the_wait_and_acquires_instantly_when_free() {
        let p = Semaphore::new(1);
        let _hold = p.try_acquire().expect("fresh semaphore has a permit");
        assert!(
            wait_for_permit(&p, Instant::now() - Duration::from_secs(1)).is_none(),
            "a passed deadline must give up, not block"
        );
        drop(_hold);
        assert!(
            wait_for_permit(&p, Instant::now() + Duration::from_secs(5)).is_some(),
            "a free permit must be acquired instantly"
        );
    }

    /// F17: two sequential calls through the real `yt_stdout` core (fake
    /// binary, exits 0) both complete — each acquires and releases the global
    /// permit.
    #[test]
    fn two_sequential_yt_stdout_calls_complete() {
        let path = fake_bin("echo", "printf ok");
        let bin = path.to_str().expect("temp path is utf-8");
        assert_eq!(
            yt_stdout_with_bin(bin, &["-J"], &[], None).as_deref(),
            Some("ok")
        );
        assert_eq!(
            yt_stdout_with_bin(bin, &["-J"], &[], None).as_deref(),
            Some("ok")
        );
        let _ = std::fs::remove_file(&path);
    }
}
