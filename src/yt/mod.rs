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
            let Some(_url) = t["url"].as_str() else {
                continue;
            };
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
            let Some(_url) = t["url"].as_str() else {
                continue;
            };
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
            if let Some(shelf) = section
                .get("musicShelfRenderer")
                .and_then(|s| s.get("contents"))
                .and_then(|c| c.as_array())
            {
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
        Some((a, al)) => (
            a.trim().to_string(),
            Some(al.trim()).filter(|s| !s.is_empty()).map(String::from),
        ),
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
    col.get("text").map(runs_text).unwrap_or_default()
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
                artist: s
                    .artists
                    .first()
                    .map(|a| a.name.clone())
                    .unwrap_or_default(),
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
