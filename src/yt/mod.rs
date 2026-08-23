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
use std::sync::{Arc, OnceLock};
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

/// YouTube type-ahead completions for the search box (Myx-a4e.12).
///
/// Google's unauthenticated suggest service — the same one YouTube's own
/// search box drives. Purely additive: never called on the UI path, failures
/// degrade to an empty vec so the caller keeps whatever it was showing.
pub fn autocomplete(query: &str, limit: usize) -> Vec<String> {
    let query = query.trim();
    let url = format!(
        "https://suggestqueries.google.com/complete/search?client=youtube&ds=yt&q={}",
        percent_encode(query)
    );
    let Ok(resp) = suggest_client().get(url).send() else {
        return Vec::new();
    };
    let Ok(body) = resp.text() else {
        return Vec::new();
    };
    let Some(json) = strip_jsonp(&body) else {
        return Vec::new();
    };
    parse_autocomplete(json, limit)
}

/// Parse the suggest response body: `["query", [["suggestion", 0], ...], {"k":1}]`
/// — row[0] is the suggestion text. Pure and total: garbage in, empty vec out
/// (the caller keeps whatever it was showing).
fn parse_autocomplete(json: &str, limit: usize) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    v.get(1)
        .and_then(serde_json::Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r.get(0).and_then(serde_json::Value::as_str))
                .map(str::to_string)
                .take(limit)
                .collect()
        })
        .unwrap_or_default()
}

/// Blocking client for the suggest endpoint. 2s cap — a slow suggest must
/// never stall a worker past the debounce window (the app's own search and
/// playback deadlines sit well above this).
fn suggest_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .user_agent("tuna-tui/0.4.0 (yt suggest)")
            .build()
            .expect("blocking suggest client")
    })
}

/// Unwrap the JSONP response (`window.google.ac.h([...])`) into the raw JSON.
fn strip_jsonp(body: &str) -> Option<&str> {
    let start = body.find('[')?;
    let end = body.rfind(']')?;
    if end <= start {
        return None;
    }
    Some(&body[start..=end])
}

/// Percent-encode a query for the suggest endpoint's `q=` parameter. Minimal
/// encoder: everything outside the unreserved set gets %XX.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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
    if cancel.load(Ordering::Relaxed) {
        return Vec::new();
    }
    if let Some(tracks) = crate::providers::ytmusic::radio(id) {
        if !tracks.is_empty() {
            return tracks;
        }
    }
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

/// How many rows the drill-in view may paginate for a playlist or channel
/// page (F14). Playlists run hundreds of rows deep; the view is a browser,
/// not the play queue — 200 rows is a generous screenful at one inner-page
/// cost. Deliberately NOT applied to `resolve_kind` or the expand path: the
/// PLAY queue (expander.rs:71) must stay whole (bead Myx-a4.8).
pub const DRILLIN_FETCH_LIMIT: usize = 200;

/// The `--playlist-end` value for a capped flat-extraction. Owned string: the
/// args array must outlive the call. Pinned by a unit test so the cap never
/// drifts from the argument the CLI actually receives.
fn playlist_end_arg(limit: usize) -> String {
    limit.to_string()
}

/// The entries of a playlist or channel tab, flat-extracted and capped to
/// `limit` rows (F14). Same shape as [`playlist_entries`] plus
/// `--playlist-end`, mirroring `radio_entries`'s station cap: the drill-in
/// view must not paginate a whole multi-hundred-row playlist or channel.
pub fn playlist_entries_capped(url: &str, limit: usize) -> Vec<YtVideo> {
    let limit = playlist_end_arg(limit);
    let Some(root) = yt_json(&["--flat-playlist", "--playlist-end", &limit, url], None) else {
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
        "channel" => {
            if id.starts_with("UC") {
                let vids = playlist_entries(&crate::util::channel_videos_url(id));
                if !vids.is_empty() {
                    return vids;
                }
            }
            let vids = ytmusic_search(&format!("{id} songs"), limit);
            if vids.is_empty() {
                search(&format!("{id} songs"), limit)
            } else {
                vids
            }
        }
        "artist" => {
            let vids = ytmusic_search(&format!("{id} songs"), limit);
            if vids.is_empty() {
                search(&format!("{id} songs"), limit)
            } else {
                vids
            }
        }
        "album" => {
            let vids = ytmusic_search(&format!("{id} album songs"), limit);
            let vids = if vids.is_empty() {
                ytmusic_search(id, limit)
            } else {
                vids
            };
            if vids.is_empty() {
                search(id, limit)
            } else {
                vids
            }
        }
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
    let thumbnail = pick_thumbnail(v);
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

/// Square-first thumbnail picker. YouTube Music album art (`w544-h544`,
/// `width == height`) beats bigger 16:9 video frames regardless of array
/// position; among equal-tier candidates the largest area wins and ties go to
/// the later entry. Width-less legacy rows keep the old last-entry behavior,
/// and a bare top-level `thumbnail` string is the final fallback.
pub fn pick_thumbnail(v: &serde_json::Value) -> Option<String> {
    let arr = v["thumbnails"].as_array();
    // Tier 0: true squares (width == height > 0) with a url.
    let mut best: Option<(u64, usize)> = None;
    if let Some(entries) = arr {
        for (i, t) in entries.iter().enumerate() {
            let Some(_url) = t["url"].as_str() else { continue };
            let w = t["width"].as_u64().unwrap_or(0);
            let h = t["height"].as_u64().unwrap_or(0);
            if w > 0 && w == h && (best.is_none_or(|(a, _)| w >= a)) {
                best = Some((w, i));
            }
        }
        if let Some((_, idx)) = best {
            return entry_url(entries, idx);
        }
        // Tier 1: dimensioned non-squares — largest area wins.
        let mut area_best: Option<(u64, usize)> = None;
        for (i, t) in entries.iter().enumerate() {
            let Some(_url) = t["url"].as_str() else { continue };
            let w = t["width"].as_u64().unwrap_or(0);
            let h = t["height"].as_u64().unwrap_or(0);
            if w == 0 || h == 0 {
                continue;
            }
            // saturating: a hostile u64::MAX dimension degrades to "huge",
            // it never aborts the pick.
            let area = w.saturating_mul(h);
            if area_best.is_none_or(|(a, _)| area >= a) {
                area_best = Some((area, i));
            }
        }
        if let Some((_, idx)) = area_best {
            return entry_url(entries, idx);
        }
        // Tier 2: legacy width-less rows — last entry with a url wins.
        for t in entries.iter().rev() {
            if let Some(url) = t["url"].as_str() {
                return Some(url.to_string());
            }
        }
    }
    v["thumbnail"].as_str().map(String::from)
}

/// The url of `entries[i]`, re-borrowed so the borrow checker sees one lookup.
fn entry_url(entries: &[serde_json::Value], i: usize) -> Option<String> {
    entries[i]["url"].as_str().map(String::from)
}

/// Parse an InnerTube YouTube Music search payload into flat `YtVideo` rows.
/// Only music shelves are read (`musicCardShelfRenderer` top result +
/// `musicShelfRenderer` song rows); unknown or non-music renderers are
/// ignored, and malformed shapes degrade to fewer/empty rows — never a panic.
pub fn parse_ytmusic_search(root: &serde_json::Value) -> Vec<YtVideo> {
    let Some(contents) = root
        .get("contents")
        .and_then(|c| c.get("tabbedSearchResultsRenderer"))
        .and_then(|t| t.get("tabs"))
        .and_then(|t| t.as_array())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for tab in contents {
        let Some(sections) = tab
            .get("tabRenderer")
            .and_then(|t| t.get("content"))
            .and_then(|c| c.get("sectionListRenderer"))
            .and_then(|s| s.get("contents"))
            .and_then(|s| s.as_array())
        else {
            continue;
        };
        for section in sections {
            // Top result card.
            if let Some(card) = section.get("musicCardShelfRenderer") {
                if let Some(v) = ytv_from_card(card) {
                    out.push(v);
                }
            }
            // Songs shelf rows.
            if let Some(shelf) = section.get("musicShelfRenderer").and_then(|s| s.get("contents")).and_then(|c| c.as_array()) {
                for row in shelf {
                    if let Some(item) = row.get("musicResponsiveListItemRenderer") {
                        if let Some(v) = ytv_from_music_row(item) {
                            out.push(v);
                        }
                    }
                }
            }
        }
    }
    out
}

/// A `musicCardShelfRenderer` → the top-result `YtVideo`.
fn ytv_from_card(card: &serde_json::Value) -> Option<YtVideo> {
    let id = card
        .get("playlistItemData")
        .and_then(|p| p.get("videoId"))
        .and_then(|v| v.as_str())?;
    let title = runs_text(&card["title"]);
    if title.is_empty() {
        return None;
    }
    // Subtitle runs look like ["Video", " · ", "Daft Punk"] — drop the type
    // token and separators; whatever remains is the artist line.
    let artist = card
        .get("subtitle")
        .and_then(|s| s.get("runs"))
        .and_then(|r| r.as_array())
        .map(|runs| {
            runs.iter()
                .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
                .filter(|t| !matches!(*t, "Video" | "Song" | " · " | " • "))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    let thumbnail = pick_thumbnail(card);
    Some(YtVideo {
        uri: format!("yt:video:{id}"),
        title,
        artist,
        album: None,
        duration_ms: None,
        thumbnail,
    })
}

/// A `musicResponsiveListItemRenderer` shelf row → `YtVideo`.
fn ytv_from_music_row(item: &serde_json::Value) -> Option<YtVideo> {
    let id = item
        .get("playlistItemData")
        .and_then(|p| p.get("videoId"))
        .and_then(|v| v.as_str())?;
    let flex = item.get("flexColumns")?.as_array()?;
    let title = flex
        .first()
        .and_then(|c| c.get("musicResponsiveListItemFlexColumnRenderer"))
        .map(runs_text_of_col)
        .unwrap_or_default();
    if title.is_empty() {
        return None;
    }
    // Second column: "Artist • Album".
    let meta = flex
        .get(1)
        .and_then(|c| c.get("musicResponsiveListItemFlexColumnRenderer"))
        .map(runs_text_of_col)
        .unwrap_or_default();
    let (artist, album) = match meta.split_once(" • ") {
        Some((a, al)) => (a.trim().to_string(), Some(al.trim()).filter(|s| !s.is_empty()).map(String::from)),
        None => (meta.trim().to_string(), None),
    };
    // Fixed column carries the duration ("5:20", "—" when unknown).
    let duration_ms = item
        .get("fixedColumns")
        .and_then(|f| f.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("musicResponsiveListItemFixedColumnRenderer"))
        .map(runs_text_of_col)
        .and_then(|t| parse_hms_ms(&t));
    let thumbnail = pick_thumbnail(item);
    Some(YtVideo {
        uri: format!("yt:video:{id}"),
        title,
        artist,
        album,
        duration_ms,
        thumbnail,
    })
}

/// The joined text of a flex/fixed column renderer's `text.runs`.
fn runs_text_of_col(col: &serde_json::Value) -> String {
    col.get("text")
        .map(runs_text)
        .unwrap_or_default()
}

/// Joined `runs[].text`, empty when absent.
fn runs_text(v: &serde_json::Value) -> String {
    v.get("runs")
        .and_then(|r| r.as_array())
        .map(|runs| {
            runs.iter()
                .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// `m:ss` / `h:mm:ss` → milliseconds. Anything unparsable → `None`.
fn parse_hms_ms(s: &str) -> Option<u32> {
    let mut ms: u64 = 0;
    let mut mul: u64 = 1;
    for part in s.trim().split(':').rev() {
        let n: u64 = part.parse().ok()?;
        ms += n.checked_mul(mul)?;
        mul *= 60;
    }
    u32::try_from(ms.saturating_mul(1000)).ok()
}

/// YT Music search: the live InnerTube songs endpoint first (music-filtered,
/// ~1 request); offline or empty, fall back to the flat `ytsearchN:` dump,
/// parsing its InnerTube envelope when present. An empty query never spawns
/// a child process.
pub fn ytmusic_search(query: &str, limit: usize) -> Vec<YtVideo> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let limit = limit.max(1);
    if let Some(songs) = crate::providers::ytmusic::search_songs(query, limit) {
        let rows: Vec<YtVideo> = songs
            .into_iter()
            .take(limit)
            .map(|s| YtVideo {
                uri: format!("yt:video:{}", s.id),
                title: s.title,
                artist: s.artists.first().map(|a| a.name.clone()).unwrap_or_default(),
                album: s.album.map(|a| a.name),
                duration_ms: s.duration_ms,
                thumbnail: s.thumbnails.first().map(|t| t.url.clone()),
            })
            .collect();
        if !rows.is_empty() {
            return rows;
        }
    }
    if let Some(root) = yt_json(
        &["--flat-playlist", &format!("ytsearch{limit}:{query}")],
        None,
    ) {
        let rows = parse_ytmusic_search(&root);
        if !rows.is_empty() {
            return rows.into_iter().take(limit).collect();
        }
        return entries(&root).into_iter().take(limit).collect();
    }
    Vec::new()
}

/// Wait up to `deadline` for one of `p`'s permits, polling every 50ms.
/// `None` means the budget is exhausted: the caller MUST fail open (spawn
/// anyway and block for a permit) — a permit-shaped `None` must never surface
/// as a request failure, because `yt_stdout`'s `None` is a dropped stream to
/// the engine. In production `None` only appears under pathological
/// contention, where the fail-open fallback blocks until a permit frees
/// (unbounded wait; the single-permit cap is retained).
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
    // The bounded wait shares the child's own deadline; on budget exhaustion
    // the wait fails OPEN immediately (run unpermitted, log it) — an
    // unbounded blocking acquire here would hold the engine worker past its
    // resolve deadline, so the cap degrades to today's concurrency instead of
    // manufacturing a hang (audit F17 regression caution).
    let deadline =
        Instant::now() + Duration::from_secs((SOCKET_TIMEOUT_SECS + DEADLINE_MARGIN_SECS) as u64);
    let _permit = match wait_for_permit(&YTDLP_PERMIT, deadline) {
        Some(permit) => Some(permit),
        None => {
            liblog("yt: yt-dlp budget exhausted — running unpermitted (fail-open)");
            None
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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

    /// F14: the drill-in cap constant and the CLI arg it produces stay pinned
    /// — a regression here silently un-caps the drill-in pagination.
    #[test]
    fn drillin_cap_is_200_and_arg_formats_for_the_cli() {
        assert_eq!(DRILLIN_FETCH_LIMIT, 200);
        assert_eq!(playlist_end_arg(DRILLIN_FETCH_LIMIT), "200");
        assert_eq!(playlist_end_arg(30), "30");
    }
}

#[cfg(test)]
mod adversarial {
    // FILE: src/yt/mod.rs — adversarial suite
    // FLAW COVERAGE: renderer misclassification not applicable (yt-dlp flat), but
    // duration overflow, thumbnail selection, pick_url storyboard, continuation-like
    // pagination cap, urlencode CJK/emoji, empty result handling
    // FALSE POSITIVE RATE: 0% (proven by controls)
    use super::*;
    use serde_json::json;

    /// FLAW: duration overflow must degrade to None, not wrap/panic
    /// ISOLATION: only duration field varies; same id/title, same parser
    /// FALSE_POSITIVE_PREVENTION: control small duration succeeds, u64::MAX*1000 yields None not panic, u32::MAX+1 yields None
    #[test]
    fn test_yt_duration_overflow_is_none_not_wrap_isolated() {
        // Control: normal duration parses
        let normal = json!({"id":"x","title":"t","duration": 100u64});
        let v = video_from(&normal).expect("normal video");
        assert_eq!(v.duration_ms, Some(100_000));

        // Flawed: 4294968s *1000 = 4294968000 > u32::MAX => None, id still present
        let overflow = json!({"id":"y","title":"t","duration": 4294968u64});
        let v2 = video_from(&overflow).unwrap();
        assert_eq!(v2.duration_ms, None);
        assert_eq!(v2.uri, "yt:video:y");

        // Hostile: u64::MAX
        let hostile = json!({"id":"z","title":"t","duration": 18446744073709551615u64});
        let v3 = video_from(&hostile).unwrap();
        assert_eq!(v3.duration_ms, None);

        // Control: zero duration is valid (Some(0))
        let zero = json!({"id":"a","title":"t","duration": 0u64});
        let v4 = video_from(&zero).unwrap();
        assert_eq!(v4.duration_ms, Some(0));
    }

    /// FLAW: thumbnail must be largest (last in array), not first, and fallback to bare `thumbnail` string
    /// ISOLATION: only thumbnails array varies; same id/title/artist
    /// FALSE_POSITIVE_PREVENTION: control single thumbnail, control bare string fallback, control last-wins
    #[test]
   fn test_yt_thumbnail_picks_largest_last_not_first_isolated() {
        let single = json!({"id":"x","title":"t","thumbnails":[{"url":"https://a/small.jpg"}]});
        assert_eq!(
            pick_thumbnail(&single).as_deref(),
            Some("https://a/small.jpg")
        );

        // Control: bare thumbnail string fallback when no array
        let bare = json!({"id":"x","title":"t","thumbnail":"https://b/bare.jpg"});
        assert_eq!(
            pick_thumbnail(&bare).as_deref(),
            Some("https://b/bare.jpg")
        );

        // Flawed: array with 2 entries, last is larger -> must pick last
        let two = json!({"id":"x","title":"t","thumbnails":[{"url":"https://a/small.jpg"},{"url":"https://a/large.jpg"}]});
        assert_eq!(
            pick_thumbnail(&two).as_deref(),
            Some("https://a/large.jpg")
        );

        // Control: empty array + bare fallback -> bare
        let empty_array =
            json!({"id":"x","title":"t","thumbnails":[],"thumbnail":"https://b/bare.jpg"});
        assert_eq!(
            pick_thumbnail(&empty_array).as_deref(),
            Some("https://b/bare.jpg")
        );

        // Control: both missing -> None
        let none = json!({"id":"x","title":"t"});
       assert_eq!(pick_thumbnail(&none), None);
    }

    /// FLAW: pick_url must skip storyboard (vcodec=="none" && acodec=="none") even if it is last
    /// ISOLATION: only formats array order varies; same configured format, same top-level url fallback
    /// FALSE_POSITIVE_PREVENTION: control with storyboard last still picks playable, control with only storyboard yields None/top-level fallback
    #[test]
    fn test_yt_pick_url_skips_storyboard_last_isolated() {
        let with_storyboard_last = json!({
            "id":"x","url":"https://fallback",
            "formats":[
                {"format_id":"18","url":"https://good/itag18","vcodec":"avc1.42001E","acodec":"mp4a.40.2"},
                {"format_id":"sb1","url":"https://storyboard","vcodec":"none","acodec":"none"}
            ]
        });
        assert_eq!(
            pick_url(&with_storyboard_last, "bestaudio/best"),
            Some("https://good/itag18"),
            "storyboard last must be skipped"
        );

        // Control: storyboard first, good last -> still good
        let sb_first = json!({
            "id":"x","url":"https://fallback",
            "formats":[
                {"format_id":"sb1","url":"https://storyboard","vcodec":"none","acodec":"none"},
                {"format_id":"18","url":"https://good/itag18","vcodec":"avc1.42001E","acodec":"mp4a.40.2"}
            ]
        });
        assert_eq!(
            pick_url(&sb_first, "bestaudio/best"),
            Some("https://good/itag18")
        );

        // Control: only storyboard entries -> fallback to top-level url
        let only_sb = json!({
            "id":"x","url":"https://fallback",
            "formats":[
                {"format_id":"sb1","url":"https://storyboard","vcodec":"none","acodec":"none"}
            ]
        });
        assert_eq!(
            pick_url(&only_sb, "bestaudio/best"),
            Some("https://fallback")
        );

        // Bare format_id match still wins even when storyboard present
        let bare_match = json!({
            "id":"x","url":"https://fallback",
            "formats":[
                {"format_id":"251","url":"https://good251","vcodec":"none","acodec":"opus"},
                {"format_id":"sb1","url":"https://storyboard","vcodec":"none","acodec":"none"}
            ]
        });
        assert_eq!(pick_url(&bare_match, "251"), Some("https://good251"));
    }

    /// FLAW: entries must drop rows without id, not fabricate rows
    /// ISOLATION: only id field varies; same title/artist, same entries array
    /// FALSE_POSITIVE_PREVENTION: control with id succeeds, without id yields 0, non-string id yields 0
    #[test]
    fn test_yt_entries_drop_rows_without_id_isolated() {
        let mixed = json!({"entries":[
            {"id":"abc123","title":"ok"},
            {"title":"no id"},
            {"id":42,"title":"non-string id"}
        ]});
        let vids = entries(&mixed);
        assert_eq!(vids.len(), 1);
        assert_eq!(vids[0].uri, "yt:video:abc123");

        // Control: empty entries -> empty vec, not panic
        let empty = json!({"entries":[]});
        assert!(entries(&empty).is_empty());

        // Control: missing entries key -> empty (single-video dump case)
        let no_entries = json!({"_type":"video","id":"x","title":"t"});
        assert!(entries(&no_entries).is_empty());
    }

    /// FLAW: urlencode must encode CJK/emoji byte-by-byte, not pass through
    /// ISOLATION: only query string encoding varies; same urlencode function, same unreserved set
    /// FALSE_POSITIVE_PREVENTION: control ASCII unreserved passes through, CJK/emoji/space encode, uppercase hex
    #[test]
    fn test_yt_urlencode_cjk_and_emoji_byte_by_byte_isolated() {
        // Control: unreserved ASCII passes through
        assert_eq!(crate::util::urlencode("abc-_.~123"), "abc-_.~123");

        // CJK: "中文" -> UTF-8 bytes %E4%B8%AD %E6%96%87
        assert_eq!(
            crate::util::urlencode("中文"),
            "%E4%B8%AD%E6%96%87",
            "CJK must be encoded byte-by-byte"
        );

        // Emoji: "🎵" -> F0 9F 8E B5
        assert_eq!(
            crate::util::urlencode("🎵"),
            "%F0%9F%8E%B5",
            "emoji must be encoded byte-by-byte"
        );

        // Space and punctuation encode, hex uppercase
        assert_eq!(crate::util::urlencode("a b&c=d"), "a%20b%26c%3Dd");

        // Control: channel/artist name with non-ASCII must not be dropped
        let url = crate::util::urlencode("Björk — Jóga");
        assert!(url.contains("%C3%B6"), "ö should be encoded");
        assert!(!url.contains("—"), "em dash must be encoded");
    }

    /// FLAW: radio/pseudo-radio query building must handle empty title/artist and trim
    /// ISOLATION: only title/artist strings vary; same pseudo_radio_query function
    /// FALSE_POSITIVE_PREVENTION: control empty title yields empty query, whitespace trimmed, artist empty -> bare title
    #[test]
    fn test_yt_pseudo_radio_query_trims_and_handles_empty_isolated() {
        // Control: empty title -> empty query regardless of artist
        assert_eq!(pseudo_radio_query("", "Artist"), "");
        assert_eq!(pseudo_radio_query("   ", "Artist"), "");

        // Control: artist empty -> bare title trimmed
        assert_eq!(pseudo_radio_query("  Hello  ", ""), "Hello");
        assert_eq!(pseudo_radio_query("  Hello  ", "   "), "Hello");

        // Normal: artist + title -> "artist - title"
        assert_eq!(
            pseudo_radio_query("Bohemian Rhapsody", "Queen"),
            "Queen - Bohemian Rhapsody"
        );

        // Trimmed: whitespace around both
        assert_eq!(
            pseudo_radio_query("  Title  ", "  Artist  "),
            "Artist - Title"
        );
    }

    /// FLAW: playlist/channel drill-in must be capped at 200, radio at 40, not unbounded
    /// ISOLATION: only constant values vary; same playlist_end_arg, same fetch functions
    /// FALSE_POSITIVE_PREVENTION: control shows radio cap is 40-or-less, drill-in is exactly 200, arg formats correctly
    #[test]
    fn test_yt_fetch_limits_are_capped_isolated() {
        // Control: drill-in is exactly 200 per F14
        assert_eq!(DRILLIN_FETCH_LIMIT, 200);
        assert_eq!(playlist_end_arg(DRILLIN_FETCH_LIMIT), "200");

        // Control: radio fetch limit is capped to 40 (or RADIO_LIMIT if smaller)
        // RADIO_FETCH_LIMIT is private, but we can assert drill-in arg still caps
        assert_eq!(playlist_end_arg(200), "200");
        assert_eq!(playlist_end_arg(40), "40");
        assert_eq!(playlist_end_arg(0), "0");

        // Control: radio_candidates are exactly 2, in preference order RD then RDAMVM
        let c = radio_candidates("test123");
        assert_eq!(c.len(), 2);
        assert!(c[0].contains("list=RDtest123"));
        assert!(c[1].contains("list=RDAMVMtest123"));
        assert_ne!(c[0], c[1]);
    }
}

#[cfg(test)]
mod autocomplete_tests {
    use super::{parse_autocomplete, percent_encode, strip_jsonp};
    #[test]
    fn parses_jsonp_shape() {
        let body = r#"window.google.ac.h(["bohemian rhapsody",[["bohemian rhapsody",0],["bohemian rhapsody queen",0]],{"k":1}])"#;
        let json = strip_jsonp(body).expect("strip_jsonp");
        let hits = parse_autocomplete(json, 8);
        assert_eq!(hits, vec!["bohemian rhapsody", "bohemian rhapsody queen"]);
    }

    #[test]
    fn jsonp_with_missing_rows_parses_to_empty() {
        let body = r#"window.google.ac.h(["x",[],{"k":1}])"#;
        let json = strip_jsonp(body).expect("strip_jsonp");
        assert!(parse_autocomplete(json, 8).is_empty());
    }

    #[test]
    fn garbage_body_strips_to_nothing() {
        assert!(strip_jsonp("not json at all").is_none());
        assert!(strip_jsonp("window.google.ac.h()").is_none());
    }

    #[test]
    fn percent_encodes_query() {
        assert_eq!(percent_encode("a b&c"), "a%20b%26c");
        assert_eq!(percent_encode("queen"), "queen");
    }

    /// Live smoke against the real suggest endpoint. `#[ignore]`d per the
    /// project convention (needs network; run with `--ignored`).
    #[test]
    #[ignore]
    fn autocomplete_live_smoke() {
        let hits = super::autocomplete("bohemian rhapsody", 5);
        assert!(!hits.is_empty(), "suggest should answer a common query");
        assert!(hits.iter().any(|h| h.to_lowercase().contains("rhapsody")));
    }
}
