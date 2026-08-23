//! Shared offline fixtures for `tests/thumbs_queue_search.rs`.
//!
//! Everything here is hermetic: no network, no real yt-dlp, no writes outside
//! a per-process directory under `/tmp/opencode`. The trick that makes the
//! `src/yt` layer testable from OUTSIDE the crate is `config::get()`'s home
//! lookup: we point `$HOME` at a scratch dir whose `config.toml` names a FAKE
//! yt-dlp shell script, then every `yt_*` call "resolves" by `cat`-ing a canned
//! JSON dump. The script dispatches on argv, so one config serves every
//! surface:
//!
//! - args contain `ytsearch`        → `search-dump.json` (flat playlist dump,
//!     DUAL-ENCODED with a minimal Innertube `contents` envelope — see below)
//! - anything else (watch/RD URLs)  → `video-dump.json`  (single-video `-J` dump
//!     with a mixed square/16:9 thumbnail array + android-shaped `formats`)
//!
//! **Why `search-dump.json` carries BOTH shapes:** plan §4.1 allows the
//! YT-Music search to land either as `yt_json(... search_music ...)` (still a
//! flat dump) or as an InnerTube parse of a music.youtube.com payload. By
//! encoding the SAME rows (same ids/titles/artists, same square art) both as
//! `entries[]` and as `contents.tabbedSearchResultsRenderer`, the tests stay
//! green whichever route the fix takes, and stay RED today (last-thumb 16:9)
//! — which is exactly the pinned bug.

use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::OnceLock;

// ────────────────────────────────────────────────────────────── canonical URLs

/// Square YT-Music album art (the `lh3.googleusercontent.com … w544-h544`
/// shape the plan calls out as "vmusic-like"). MUST win over the 16:9 frames.
pub const SQ_QUEEN: &str = "https://lh3.googleusercontent.com/QUEEN-SQUARE=w544-h544-l90-rj";
/// 16:9 video frame that comes AFTER the square art in the arrays (today's
/// `largest_thumbnail` takes the last entry → this URL — the bug).
pub const WIDE_QUEEN_HQ720: &str = "https://i.ytimg.com/vi/fJ9rUzIMcZQ/hq720.jpg";
pub const WIDE_QUEEN_MAXRES: &str = "https://i.ytimg.com/vi/dQw4w9WgXcQ/maxresdefault.jpg";
pub const WIDE_DQ_HQDEFAULT: &str = "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg";
/// Legacy width-less single frame (Survivor row) — must keep working verbatim.
pub const LEGACY_SURVIVOR: &str = "https://i.ytimg.com/vi/l9nh1l8ZIJQ/hqdefault.jpg";

pub const ID_QUEEN: &str = "fJ9rUzIMcZQ";
pub const ID_RICK: &str = "dQw4w9WgXcQ";

// ─────────────────────────────────────────────────────────── hermetic $HOME

/// Per-process scratch world: temp `$HOME`, config naming the fake yt-dlp, and
/// the two canned dumps. Created once; every test grabs it first so that
/// `config::get()`'s `OnceLock` initializes against THIS home, never the real
/// user's.
#[allow(dead_code)] // `root` documents the sandbox; some tests only need the log
pub struct Hermetic {
    /// Fake `$HOME`.
    pub root: PathBuf,
    /// Append-only invocation log the fake yt-dlp touches on every spawn
    /// (lets tests prove a child did or did NOT run).
    pub fake_log: PathBuf,
}

static HERMETIC: OnceLock<Hermetic> = OnceLock::new();

pub fn hermetic() -> &'static Hermetic {
    HERMETIC.get_or_init(|| {
        let root = std::env::temp_dir()
            .join("opencode")
            .join(format!("tuna-thumbs-fixtures-{}", std::process::id()));
        let conf_dir = root.join(".config/tuna-tui");
        std::fs::create_dir_all(&conf_dir).expect("create scratch config dir");

        let search_dump = root.join("search-dump.json");
        let video_dump = root.join("video-dump.json");
        std::fs::write(&search_dump, search_dump_json().to_string())
            .expect("write search-dump.json");
        std::fs::write(&video_dump, video_dump_json().to_string()).expect("write video-dump.json");

        let sd = search_dump.to_string_lossy().into_owned();
        let vd = video_dump.to_string_lossy().into_owned();
        let fake_log = root.join("invocations.log");
        let fl = fake_log.to_string_lossy().into_owned();
        let fake_bin = root.join("fake-yt-dlp.sh");
        std::fs::write(
            &fake_bin,
            format!(
                "#!/bin/sh\n\
                 # Fake yt-dlp: logs the invocation (baked absolute path — no\n\
                 # env indirection), then cats the canned dump selected by argv\n\
                 # shape. `exec cat` keeps pipes draining and exits instantly.\n\
                 : >> '{fl}'\n\
                 case \"$*\" in\n\
                 \x20 *ytsearch*) exec cat '{sd}' ;;\n\
                 \x20 *) exec cat '{vd}' ;;\nesac\n",
                sd = sd,
                vd = vd,
                fl = fl,
            ),
        )
        .expect("write fake-yt-dlp.sh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_bin, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake-yt-dlp.sh");
        }

        // Minimal config: only the yt-dlp seam is redirected; every other key
        // falls back to Config::default() (bestaudio/best, search_limit 6 …).
        std::fs::write(
            conf_dir.join("config.toml"),
            format!(
                "ytdlp_path = \"{}\"\n",
                fake_bin.to_string_lossy().into_owned()
            ),
        )
        .expect("write config.toml");

        // Point the crate's notion of home at the scratch dir BEFORE any test
        // reaches config::get()/liblog. Edition 2021: set_var is a safe fn.
        std::env::set_var("HOME", &root);

        Hermetic { root, fake_log }
    })
}

/// How many times the fake yt-dlp was spawned so far.
pub fn invocations(h: &Hermetic) -> usize {
    std::fs::read_to_string(&h.fake_log)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

// ──────────────────────────────────────────────────────── JSON micro-builders

/// One `thumbnails[]` entry.
pub fn thumb(url: &str, width: u32, height: u32) -> Value {
    json!({"url": url, "width": width, "height": height})
}

/// A width-less entry (legacy yt-dlp flat rows).
pub fn thumb_bare(url: &str) -> Value {
    json!({"url": url})
}

// ──────────────────────────────────────────────────────────── search dump

/// Flat `ytsearchN:` dump **dual-encoded** with a minimal Innertube envelope.
///
/// Rows (identical identity in both encodings):
/// 1. Queen — Bohemian Rhapsody …  channel `Queen Official`, square art FIRST,
///    wide 720p art LAST (today's picker picks the wide one ⇒ pinned bug);
/// 2. Survivor — Eye Of The Tiger … NO channel, ONE width-less frame (legacy
///    pin: must survive the fix byte-for-byte);
/// 3. id-only row with NO title/NO channel/NO thumbs (edge: truly unknown).
pub fn search_dump_json() -> Value {
    let queen_flat = json!({
        "_type": "url", "id": ID_QUEEN,
        "title": "Queen – Bohemian Rhapsody (Official Video Remastered)",
        "channel": "Queen Official",
        "uploader": "Queen Official",
        "duration": 360,
        "thumbnails": [
            thumb(SQ_QUEEN, 544, 544),
            thumb(WIDE_QUEEN_HQ720, 1280, 720)
        ]
    });
    let survivor_flat = json!({
        "_type": "url", "id": "l9nh1l8ZIJQ",
        "title": "Survivor - Eye Of The Tiger (Official HD Video)",
        "duration": 245,
        "thumbnails": [thumb_bare(LEGACY_SURVIVOR)]
    });
    let untitled_flat = json!({ "_type": "url", "id": "notitle00001" });

    json!({
        "_type": "playlist", "id": "search",
        "title": "bohemian rhapsody queen",
        // ── encoding B: minimal Innertube payload the §4.1b parser reads ──
        "contents": { "tabbedSearchResultsRenderer": { "tabs": [ { "tabRenderer": {
            "selected": true,
            "content": { "sectionListRenderer": { "contents": [
                { "musicShelfRenderer": { "title": { "runs": [{"text": "Songs"}] },
                  "contents": [
                    responsive_row(
                        ID_QUEEN,
                        "Queen – Bohemian Rhapsody (Official Video Remastered)",
                        "Queen Official", "Greatest Hits", "4:08",
                        &[thumb(SQ_QUEEN, 544, 544), thumb(WIDE_QUEEN_HQ720, 1280, 720)]
                    ),
                    responsive_row(
                        "l9nh1l8ZIJQ",
                        "Survivor - Eye Of The Tiger (Official HD Video)",
                        "", "", "",
                        &[thumb_bare(LEGACY_SURVIVOR)]
                    ),
                    // The truly-unknown row mirrors too, so both routes see it.
                    responsive_row("notitle00001", "", "", "", "", &[])
                  ] } }
            ] } }
        } } ] } },
        // ── encoding A: the flat entries the current parser reads ──
        "entries": [queen_flat, survivor_flat, untitled_flat]
    })
}

/// One `musicResponsiveListItemRenderer` (the shape §4.1b maps to `YtVideo`).
#[allow(clippy::too_many_arguments)]
pub fn responsive_row(
    video_id: &str,
    title: &str,
    artist: &str,
    album: &str,
    duration: &str,
    thumbnails: &[Value],
) -> Value {
    let mut cols = vec![json!({ "musicResponsiveListItemFlexColumnRenderer": {
        "text": { "runs": [{ "text": title }] } } })];
    // Second column: "Artist • Album" run pair (either part may be empty).
    let mut meta_runs = Vec::new();
    if !artist.is_empty() {
        meta_runs.push(json!({ "text": artist }));
    }
    if !album.is_empty() {
        if !meta_runs.is_empty() {
            meta_runs.push(json!({ "text": " • " }));
        }
        meta_runs.push(json!({ "text": album }));
    }
    cols.push(json!({ "musicResponsiveListItemFlexColumnRenderer": {
        "text": { "runs": meta_runs } } }));

    let mut row = json!({
        "playlistItemData": { "videoId": video_id },
        "flexColumns": cols,
        "thumbnail": { "musicThumbnailRenderer": { "thumbnail": {
            "thumbnails": thumbnails } } }
    });
    if !duration.is_empty() {
        row["fixedColumns"] = json!([{ "musicResponsiveListItemFixedColumnRenderer": {
            "displayPriority": "MUSIC_RESPONSIVE_LIST_ITEM_COLUMN_DISPLAY_PRIORITY_HIGH",
            "text": { "runs": [{ "text": duration }] } } }]);
    }
    row
}

// ──────────────────────────────────────────────────────────── video dump

/// Single-video `-J` dump consumed by `video_meta` / `resolve` /
/// `resolve_stream`. The thumbnail array interleaves square album art BETWEEN
/// two 16:9 frames, with the biggest 16:9 LAST — the exact layout that makes
/// today's `largest_thumbnail` (last-wins) show `maxresdefault` instead of the
/// square music art. `formats` reproduces the android-client ordering quirk
/// (storyboard first, playable mux last) that `pick_url` must navigate.
pub fn video_dump_json() -> Value {
    json!({
        "_type": "video", "id": ID_RICK,
        "title": "Rick Astley - Never Gonna Give You Up (Official Video)",
        "channel": "Rick Astley", "uploader": "Rick Astley",
        "duration": 213,
        "webpage_url": "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        "thumbnails": [
            thumb(WIDE_DQ_HQDEFAULT, 480, 360),
            thumb(SQ_RICK, 544, 544),
            thumb(WIDE_QUEEN_MAXRES, 1280, 720)
        ],
        "artist": null, "album": null,
        "url": "https://googlevideo.example/videoplayback?itag=18&test=1",
        "formats": [
            { "format_id": "sb1",
              "url": "https://i.ytimg.com/sb/dQw4w9WgXcQ/storyboard3_L1",
              "vcodec": "none", "acodec": "none" },
            { "format_id": "18",
              "url": "https://googlevideo.example/videoplayback?itag=18",
              "vcodec": "avc1.42001E", "acodec": "mp4a.40.2" }
        ]
    })
}

pub const SQ_RICK: &str = "https://lh3.googleusercontent.com/RICK-SQUARE=w544-h544-l90-rj";

// ───────────────────────────────────────────────── pure Innertube fixtures ─┐
// Used only by the `music_search` module against `yt::parse_ytmusic_search`. │
// These follow implementation_spec §10/§62–65 field paths; they double as    │
// the parser's versioned fixture per vitune-plan §59.                        │
// ────────────────────────────────────────────────────────────────────────────

/// Canonical Daft Punk capture (plan §2.2 ids), trimmed: a `musicCardShelfRenderer`
/// top result + a `musicShelfRenderer` with four songs (two duration edge rows),
/// one non-music noise renderer, and one unknown-future renderer that parsers
/// must ignore.
pub fn innertube_daft_punk() -> Value {
    let sq = |name: &str| {
        json!({ "url": format!("https://lh3.googleusercontent.com/{name}=w544-h544-l90-rj"),
                "width": 544, "height": 544 })
    };
    let wide = |id: &str| {
        json!({ "url": format!("https://i.ytimg.com/vi/{id}/hq720.jpg"),
                "width": 1280, "height": 720 })
    };

    json!({
      "contents": { "tabbedSearchResultsRenderer": { "tabs": [ { "tabRenderer": {
        "selected": true,
        "content": { "sectionListRenderer": { "contents": [
          { "musicCardShelfRenderer": {
              "title": { "runs": [{ "text": "Get Lucky" }] },
              "subtitle": { "runs": [
                  { "text": "Video" }, { "text": " · " }, { "text": "Daft Punk" } ] },
              "playlistItemData": { "videoId": "khnokW3Mw24" },
              "thumbnail": { "musicThumbnailRenderer": { "thumbnail": { "thumbnails": [
                  sq("GETLUCKY"), wide("khnokW3Mw24") ] } } } } },
          { "musicShelfRenderer": { "title": { "runs": [{ "text": "Songs" }] },
              "contents": [
                responsive_row("F94s6rPnwfU", "One More Time", "Daft Punk",
                               "Discovery", "5:20",
                               &[sq("ONEMORETIME"), wide("F94s6rPnwfU")]),
                responsive_row("Kk9IBQvUfQc", "Digital Love", "Daft Punk",
                               "Discovery", "4:58",
                               &[wide("Kk9IBQvUfQc"), sq("DIGITALLOVE")]),
                // Duration column unparsable → duration_ms must stay None.
                responsive_row("durtol000001", "Too Long (radio edit)", "Daft Punk",
                               "Discovery", "—",
                               &[sq("TOOLONG")]),
                // Duration column absent entirely → None as well.
                responsive_row("nofixedcol001", "Something About Us", "Daft Punk",
                               "Discovery", "",
                               &[sq("SOMETHING")])
              ] } },
          // Non-music renderer: generic YouTube videos. Must be FILTERED OUT
          // (plan §7: “filters non-music renderer types (video vs song)”).
          { "videoShelfRenderer": { "contents": [
              responsive_row("vlogvid00001", "Daily Vlog 🎬 — NOT MUSIC",
                             "", "", "12:34",
                             &[thumb("https://example.com/vlog.jpg", 1280, 720)]) ] } },
          // Unknown future renderer: ignore silently, never panic (§6 drift).
          { "brandNewShelf2077": { "surprise": [1, 2, 3] } }
        ] } }
      } } ] } }
    })
}

// Square-art URL consts matching the fixtures above (assertions read these).
pub fn sq_url(name: &str) -> String {
    format!("https://lh3.googleusercontent.com/{name}=w544-h544-l90-rj")
}

/// Malformed Innertube payloads: the parser answers each with an EMPTY vec and
/// never panics (§6: BAD_RESPONSE → fallback, never fail search).
pub fn malformed_roots() -> Vec<Value> {
    vec![
        Value::Null,
        json!([]),
        json!({}),
        json!({ "contents": 42 }),
        json!({ "contents": { "tabbedSearchResultsRenderer": null } }),
        json!({ "contents": { "tabbedSearchResultsRenderer": { "tabs": "three" } } }),
        json!({ "contents": { "tabbedSearchResultsRenderer": { "tabs": [
            { "tabRenderer": { "content": { "sectionListRenderer": { "contents": [
                { "musicShelfRenderer": { "contents": [
                    { "playlistItemData": {} },                       // no videoId
                    { "flexColumns": "gone" }                         // wrong shape
                ] } } ] } } } } ] } } }),
    ]
}

/// `n` copies of a valid shelf row — the boundedness/perf smoke payload.
pub fn inflated_innertube(n: usize) -> Value {
    let mut shelf = Vec::with_capacity(n);
    for i in 0..n {
        shelf.push(responsive_row(
            &format!("bulk{i:07}"),
            &format!("Bulk Track {i}"),
            "Bulk Artist",
            "Bulk Album",
            "3:00",
            &[thumb(
                "https://lh3.googleusercontent.com/BULK=w544-h544",
                544,
                544,
            )],
        ));
    }
    json!({ "contents": { "tabbedSearchResultsRenderer": { "tabs": [ { "tabRenderer": {
        "content": { "sectionListRenderer": { "contents": [
            { "musicShelfRenderer": { "contents": shelf } } ] } } } } ] } } })
}
