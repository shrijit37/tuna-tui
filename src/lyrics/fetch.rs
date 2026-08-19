//! Lyric fetching, from lrclib.net — the only network-dependent lyrics source.
//!
//! Keyed on artist/title/album/duration rather than a provider track id, so it
//! survived the Spotify→YouTube port untouched: only the metadata source above
//! changed, and the key fields (`yt:` video title/channel/duration) feed the
//! same query. This is the `src/api/lyrics.rs` body, relocated into the library
//! so it outlives the bin-side api layer.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, OnceLock};

use crate::util::urlencode;

/// One memo entry: `(lines, synced)` — same shape `fetch_lyrics_blocking`
/// returns. A named alias keeps the MEMO static's type readable (and clippy
/// `type_complexity` quiet).
type MemoValue = (Vec<(u32, String)>, bool);

/// One client for the whole process: lrclib fetches are rare (one per track
/// change) but each used to build a fresh client — TLS setup + connection
/// pool per request. `reqwest::blocking::Client` is `Send + Sync`, so a
/// shared instance is sound across the worker threads.
static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();

/// Session-scoped memo of lrclib results, keyed on the exact request URL
/// (F12). Repeated tracks — the same song again, radio loops — used to
/// re-fetch identical content on every track change; the memo kills the
/// duplicate roundtrip. Session scope on purpose: entries die at relaunch,
/// so lyrics added upstream since the last run are picked up (no
/// never-cache-empty trap, no TTL needed).
static MEMO: LazyLock<Mutex<HashMap<String, MemoValue>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Fetch lyrics for a track from lrclib. Returns `(lines, synced)`; synced
/// lines carry timestamps, plain text has none. An empty first half means no
/// match — the caller renders the view empty.
///
/// Queries the `/api/search` list endpoint, not the exact-duration `/api/get`
/// one: YouTube video lengths drift from the release durations lrclib
/// indexes, so an exact match (nearest second, often the beat on this box)
/// misses. [`pick_search_match`] accepts the record whose length is nearest
/// the video's within [`DURATION_TOLERANCE_S`] instead (Myx-a4e.7).
pub fn fetch_lyrics_blocking(
    artist: &str,
    title: &str,
    album: &str,
    duration_ms: u32,
) -> (Vec<(u32, String)>, bool) {
    let client = CLIENT.get_or_init(|| crate::httpcache::blocking_client().clone());
    let url = search_url(artist, title, album);
    fetch_lyrics_memo(client, &url, duration_ms as f64 / 1000.0)
}

/// Build the lrclib search URL for artist/title(/album). The album is
/// appended only when we actually have one — an empty album_name parameter
/// would over-constrain the search to untitled records.
fn search_url(artist: &str, title: &str, album: &str) -> String {
    let mut url = format!(
        "https://lrclib.net/api/search?artist_name={}&track_name={}",
        urlencode(artist),
        urlencode(title),
    );
    if !album.is_empty() {
        url.push_str(&format!("&album_name={}", urlencode(album)));
    }
    url
}

/// How far a search candidate's length may drift from the video's (in
/// seconds) and still be the lyrics for this track. Wide enough for the
/// release-vs-video gaps this port introduced, narrow enough to keep a
/// same-titled cover off the result.
const DURATION_TOLERANCE_S: f64 = 10.0;

/// Pick the record from an lrclib `/api/search` response whose `duration`
/// (seconds, float) is nearest `expected_duration_s`, but only within
/// [`DURATION_TOLERANCE_S`]. Returns `None` on a non-array response, when no
/// record carries a duration, or when every candidate is out of tolerance.
fn pick_search_match(
    search: &serde_json::Value,
    expected_duration_s: f64,
) -> Option<&serde_json::Value> {
    let arr = search.as_array()?;
    arr.iter()
        .filter_map(|v| v["duration"].as_f64().map(|d| (d, v)))
        .filter(|(d, _)| (d - expected_duration_s).abs() <= DURATION_TOLERANCE_S)
        .min_by(|(a, _), (b, _)| {
            (a - expected_duration_s)
                .abs()
                .total_cmp(&(b - expected_duration_s).abs())
        })
        .map(|(_, v)| v)
}

/// The memo wrapper: identical requests (same URL) are served from memory —
/// the network legs of [`fetch_lyrics_url`] never run twice for one track in
/// one session. The client is injected so tests can point the miss path at an
/// offline endpoint without touching the real lrclib.net. Never holds the
/// lock across the network fetch (a memo miss must not serialize fetches).
///
/// `expected_duration_s` is the video length in seconds; search responses are
/// arrays and [`pick_search_match`] selects the record whose length is
/// nearest it (single-record responses ignore it). Memoization is keyed on
/// the request URL alone: a given artist/title/album searches once per
/// session, regardless of how the beat length drifts between plays.
fn fetch_lyrics_memo(
    client: &reqwest::blocking::Client,
    url: &str,
    expected_duration_s: f64,
) -> (Vec<(u32, String)>, bool) {
    if let Some(hit) = MEMO.lock().unwrap_or_else(|p| p.into_inner()).get(url) {
        return hit.clone();
    }
    let result = fetch_lyrics_url(client, url, expected_duration_s);
    MEMO.lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(url.to_string(), result.clone());
    result
}

/// One lrclib GET + parse — the network core, split out of the memo wrapper
/// so the cache path is testable without real network (F12). `synced` lines
/// carry timestamps, plain text has none; an empty first half means no match.
///
/// `/api/search` (the production URL, see [`fetch_lyrics_blocking`]) answers
/// with an array: [`pick_search_match`] narrows it by duration before the
/// lyrics are read. A single-record response (the old `/api/get` shape) is
/// used as-is — the offline tests lean on that branch.
fn fetch_lyrics_url(
    client: &reqwest::blocking::Client,
    url: &str,
    expected_duration_s: f64,
) -> (Vec<(u32, String)>, bool) {
    let Ok(resp) = client
        .get(url)
        .header("User-Agent", "tuna-tui (terminal music player)")
        .send()
    else {
        return (Vec::new(), false);
    };
    if !resp.status().is_success() {
        return (Vec::new(), false);
    }
    let Ok(v) = resp.json::<serde_json::Value>() else {
        return (Vec::new(), false);
    };
    let Some(record) = pick_search_match(&v, expected_duration_s).or_else(|| {
        if v.is_array() {
            None
        } else {
            Some(&v)
        }
    }) else {
        return (Vec::new(), false);
    };
    lyrics_from_record(record)
}

/// Read `syncedLyrics` (preferred) or `plainLyrics` off one lrclib record.
fn lyrics_from_record(record: &serde_json::Value) -> (Vec<(u32, String)>, bool) {
    if let Some(synced) = record["syncedLyrics"].as_str().filter(|s| !s.is_empty()) {
        return (crate::lyrics::parse::parse_lrc(synced), true);
    }
    if let Some(plain) = record["plainLyrics"].as_str().filter(|s| !s.is_empty()) {
        let lines = plain.lines().map(|l| (0u32, l.to_string())).collect();
        return (lines, false);
    }
    (Vec::new(), false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Serve one HTTP response for a single request on `port`. The listener
    /// owns the port; the thread lives until the request arrives.
    fn serve_once(port: u16, body: &'static str) {
        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf);
                let _ = sock.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .as_bytes(),
                );
            }
        });
    }

    /// Reserve a port for a canned response and hand back its URL.
    fn canned_url(body: &'static str) -> String {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        serve_once(port, body);
        format!("http://127.0.0.1:{port}/api/search?artist_name=a&track_name=b")
    }

    /// The memo must serve a second identical request without touching the
    /// network. The first call caches the (empty) miss against a dead port;
    /// a server on the same port then serves real lyrics, and the second
    /// call — same URL — still returns the CACHED empty value, proving no
    /// re-fetch. Exactly one memo key after both calls.
    #[test]
    fn memo_serves_a_repeat_request_without_refetch() {
        // Reserve a port, then close it: the first call's URL is unreachable.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let url = format!(
            "http://127.0.0.1:{port}/api/get?artist_name=a&track_name=b&album_name=c&duration=1"
        );
        let client = reqwest::blocking::Client::new();

        // Call 1 — connection refused: the miss result is memoized.
        assert_eq!(fetch_lyrics_memo(&client, &url, 1.0), (Vec::new(), false));
        assert_eq!(MEMO.lock().unwrap().len(), 1);

        // Serve real lyrics on the very same URL now.
        serve_once(
            port,
            r#"{"syncedLyrics":"[00:01.00]hello there","plainLyrics":null}"#,
        );

        // Call 2 — identical args: the memo returns the cached miss and never
        // touches the (now live) server; a real fetch would return lyrics.
        assert_eq!(fetch_lyrics_memo(&client, &url, 1.0), (Vec::new(), false));
        assert_eq!(MEMO.lock().unwrap().len(), 1);
    }

    /// The picker must return the record whose duration is nearest the
    /// expected one — not merely "any record inside the tolerance", and not
    /// the first array element. A record with no duration field must not win
    /// (or panic) either.
    #[test]
    fn search_match_picks_duration_nearest_within_tolerance() {
        let search = json!([
            { "trackName": "far out", "duration": 88.0, "plainLyrics": "no" },
            { "trackName": "no duration", "plainLyrics": "ghost" },
            { "trackName": "winner", "duration": 96.0, "syncedLyrics": "[00:01.00]yes" },
            { "trackName": "farther", "duration": 107.0, "syncedLyrics": "[00:01.00]also ok" },
        ]);
        let picked = pick_search_match(&search, 100.0).expect("a candidate is in range");
        assert_eq!(picked["trackName"], "winner");
    }

    /// The window is inclusive at exactly ±10 s: a candidate exactly at the
    /// boundary is in, a hair past it is out. Pins the `<=` semantics a
    /// hostile review might misread as `<`.
    #[test]
    fn search_match_boundary_is_inclusive_at_exactly_ten_seconds() {
        let at_boundary = json!([
            { "trackName": "edge", "duration": 110.0, "plainLyrics": "x" },
        ]);
        let picked = pick_search_match(&at_boundary, 100.0).expect("exactly 10 s off is in");
        assert_eq!(picked["trackName"], "edge");

        let past_boundary = json!([
            { "trackName": "past", "duration": 110.000001, "plainLyrics": "x" },
        ]);
        assert!(pick_search_match(&past_boundary, 100.0).is_none());
    }

    /// Equidistant candidates (95 s and 105 s vs a 100 s video) tie on
    /// distance; the picker keeps the first in array order, i.e. lrclib's
    /// own ordering — no synced-over-plain preference is introduced, which
    /// the bead doesn't call for.
    #[test]
    fn search_match_tie_breaks_to_first_candidate_in_array_order() {
        let search = json!([
            { "trackName": "first", "duration": 95.0, "plainLyrics": "x" },
            { "trackName": "second", "duration": 105.0, "syncedLyrics": "[00:01.00]y" },
        ]);
        let picked = pick_search_match(&search, 100.0).expect("both candidates tie in range");
        assert_eq!(picked["trackName"], "first");
    }

    /// Every candidate outside the ±10 s window is a miss — a same-titled
    /// cover or a live take must not masquerade as this track.
    #[test]
    fn search_match_rejects_candidates_outside_tolerance() {
        let search = json!([
            { "trackName": "close but no", "duration": 89.0, "plainLyrics": "x" },
            { "trackName": "close but no 2", "duration": 111.0, "plainLyrics": "y" },
        ]);
        assert!(pick_search_match(&search, 100.0).is_none());
    }

    /// F6's catch (R1): among several in-tolerance candidates the picker
    /// must return the NEAREST to the expected length, not the smallest one.
    /// 90.5 is 9.5 s off and sorts first; 96.0 is 4.0 s off — the nearest
    /// wins even though it is the larger duration.
    #[test]
    fn search_match_prefers_nearest_not_smallest_in_tolerance() {
        let search = json!([
            { "trackName": "smaller", "duration": 90.5, "syncedLyrics": "[00:01.00]x" },
            { "trackName": "nearest", "duration": 96.0, "syncedLyrics": "[00:01.00]y" },
        ]);
        let picked = pick_search_match(&search, 100.0).expect("a candidate is in range");
        assert_eq!(picked["trackName"], "nearest");
    }

    /// R2 (F6's review): the search URL carries album only when one is known
    /// — an empty album must not over-constrain the query.
    #[test]
    fn search_url_includes_album_only_when_non_empty() {
        assert_eq!(
            search_url("a b", "c d", ""),
            "https://lrclib.net/api/search?artist_name=a%20b&track_name=c%20d"
        );
        assert_eq!(
            search_url("a b", "c d", "e f"),
            "https://lrclib.net/api/search?artist_name=a%20b&track_name=c%20d&album_name=e%20f"
        );
    }

    /// A non-array response (or an array with no usable record) is a miss.
    #[test]
    fn search_match_ignores_non_array_response() {
        assert!(pick_search_match(&json!({"error": "nope"}), 100.0).is_none());
        assert!(pick_search_match(&json!([]), 100.0).is_none());
        assert!(pick_search_match(&json!([{"trackName": "no duration"}]), 100.0).is_none());
    }

    /// End to end against a canned `/api/search` array: the duration-nearest
    /// in-range record's lyrics come back. The 88 s record (12 s off) and a
    /// duration-less ghost must lose to the 96 s one.
    #[test]
    fn fetch_lyrics_url_returns_duration_nearest_search_result() {
        let url = canned_url(
            r#"[{"trackName":"far","duration":88.0,"plainLyrics":"no"},
                {"trackName":"ghost","plainLyrics":"not a candidate"},
                {"trackName":"winner","duration":96.0,"syncedLyrics":"[00:01.00]yes it is"},
                {"trackName":"farther","duration":107.0,"syncedLyrics":"[00:01.00]second best"}]"#,
        );
        let client = reqwest::blocking::Client::new();
        assert_eq!(
            fetch_lyrics_url(&client, &url, 100.0),
            (vec![(1000, "yes it is".to_string())], true)
        );
    }

    /// When nothing is within tolerance the fetch is a miss, exactly like the
    /// old exact-duration query missing.
    #[test]
    fn fetch_lyrics_url_returns_empty_when_no_candidate_in_tolerance() {
        let url = canned_url(
            r#"[{"trackName":"too short","duration":80.0,"plainLyrics":"x"},
                {"trackName":"too long","duration":120.0,"plainLyrics":"y"}]"#,
        );
        let client = reqwest::blocking::Client::new();
        assert_eq!(fetch_lyrics_url(&client, &url, 100.0), (Vec::new(), false));
    }

    /// A single-record response (the old `/api/get` shape) still serves its
    /// lyrics; the expected duration only governs array picks.
    #[test]
    fn fetch_lyrics_url_falls_back_to_a_single_record_response() {
        let url = canned_url(r#"{"syncedLyrics":"[00:02.00]lone wolf","plainLyrics":null}"#);
        let client = reqwest::blocking::Client::new();
        assert_eq!(
            fetch_lyrics_url(&client, &url, 999.0),
            (vec![(2000, "lone wolf".to_string())], true)
        );
    }
}
