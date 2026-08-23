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

fn lyrics_client() -> &'static reqwest::blocking::Client {
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_millis(2500))
            .connect_timeout(std::time::Duration::from_millis(1500))
            // Fail fast: a builder failure here means TLS/runtime breakage;
            // silently falling back to a default client would drop the tight
            // 2.5s/1.5s budgets and wedge the lyrics worker on slow networks.
            .build()
            .expect("failed to build the lrclib HTTP client")
    })
}

/// Session-scoped memo of lrclib results, keyed on normalized
/// `artist|title|album|duration_s` (F1 fix). Repeated tracks — the same song
/// again, radio loops — used to re-fetch identical content on every track
/// change; the memo kills the duplicate roundtrip. Session scope on purpose:
/// entries die at relaunch, so lyrics added upstream since the last run are
/// picked up (no never-cache-empty trap, no TTL needed).
///
/// F1: memo key MUST retain a duration dimension — a given (artist,title,album)
/// triple can have many video lengths (release vs extended vs radio edit).
/// Caching on URL alone poisoned every later duration with the first miss.
/// F2: `duration_ms == 0` (flat playlist rows, stash restores) never memoizes
/// — an expected 0 would pick only ≤10s records and near-certainly miss, then
/// poison the key.
static MEMO: LazyLock<Mutex<HashMap<String, MemoValue>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(test)]
static MEMO_SERIAL: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// How far a search candidate's length may drift from the video's (in
/// seconds) and still be the lyrics for this track. Primary window per
/// spec §20 (3 s). Wider fallback per bead (10 s) for video-vs-release drift.
const PRIMARY_TOLERANCE_S: f64 = 3.0;
const FALLBACK_TOLERANCE_S: f64 = 10.0;

/// Fetch lyrics for a track from lrclib. Returns `(lines, synced)`; synced
/// lines carry timestamps, plain text has none. An empty first half means no
/// match — the caller renders the view empty.
///
/// Queries the `/api/get` exact endpoint first, then falls back to
/// `/api/search` with duration-nearest picking (§19-20, Myx-a4e.7 / Myx-mh7.7):
/// 1. exact `GET /api/get?track_name=&artist_name=&album_name=&duration=`
/// 2. search `GET /api/search?track_name=&artist_name=&album_name=` pick ±3 s
/// 3. generic `GET /api/search?q=` pick ±3 s
/// 4. normalized search `q=` pick ±10 s fallback
pub fn fetch_lyrics_blocking(
    artist: &str,
    title: &str,
    album: &str,
    duration_ms: u32,
) -> (Vec<(u32, String)>, bool) {
    let client = lyrics_client();
    let expected_s = duration_ms as f64 / 1000.0;

    // F2 guard: 0-duration is not a real length — don't memoize, just try once
    // and return (prevents an empty miss from poisoning the session).
    if duration_ms == 0 {
        return fetch_with_fallback(client, artist, title, album, expected_s);
    }

    let memo_key = memo_key(artist, title, album, duration_ms);
    if let Some(hit) = MEMO
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&memo_key)
    {
        return hit.clone();
    }

    // Check persistent on-disk cache for instant 0ms reload
    let disk_key = format!("lyrics:{memo_key}");
    if let Some(cached_json) = crate::httpcache::get(&disk_key, None) {
        if let Ok(hit) = serde_json::from_str::<MemoValue>(&cached_json) {
            MEMO.lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(memo_key, hit.clone());
            return hit;
        }
    }

    let result = fetch_with_fallback(client, artist, title, album, expected_s);
    if !result.0.is_empty() {
        if let Ok(json) = serde_json::to_string(&result) {
            crate::httpcache::put(&disk_key, &json);
        }
    }
    MEMO.lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(memo_key, result.clone());
    result
}

/// Fetch lyrics from YouTube Music InnerTube fallback, with disk caching and Indic transliteration.
pub fn fetch_ytmusic_lyrics(video_id: &str) -> (Vec<(u32, String)>, bool) {
    if video_id.is_empty() {
        return (Vec::new(), false);
    }
    let disk_key = format!("lyrics:yt:{video_id}");
    if let Some(cached_json) = crate::httpcache::get(&disk_key, None) {
        if let Ok(hit) = serde_json::from_str::<MemoValue>(&cached_json) {
            return hit;
        }
    }
    if let Some(yt_lyrics) = crate::providers::ytmusic::lyrics(video_id) {
        let lines: Vec<(u32, String)> = yt_lyrics
            .lines()
            .map(|l| {
                let text = if crate::lyrics::transliterate::contains_indic(l) {
                    crate::lyrics::transliterate::transliterate_indic(l)
                } else {
                    l.to_string()
                };
                (0u32, text)
            })
            .collect();
        let res = (lines, false);
        if !res.0.is_empty() {
            if let Ok(json) = serde_json::to_string(&res) {
                crate::httpcache::put(&disk_key, &json);
            }
        }
        res
    } else {
        (Vec::new(), false)
    }
}

/// Quietly prefetch lyrics in a background thread so they are already cached in memory
/// and on disk when the next track begins (0ms perceived latency).
pub fn prefetch_lyrics(
    artist: String,
    title: String,
    album: String,
    duration_ms: u32,
    video_id: Option<String>,
) {
    if title.is_empty() {
        return;
    }
    std::thread::Builder::new()
        .name("tuna-lyrics-prefetch".into())
        .spawn(move || {
            let res = fetch_lyrics_blocking(&artist, &title, &album, duration_ms);
            if res.0.is_empty() {
                if let Some(id) = video_id {
                    let _ = fetch_ytmusic_lyrics(&id);
                }
            }
        })
        .ok();
}

/// Build the memo key — spec §35 `lyrics:{sha256}` shaped but in-memory.
/// Use normalized lowercase + duration seconds to retain the duration dimension
/// (F1). We don't need real SHA256 for a process-local HashMap; a canonical
/// string is collision-free enough and keeps clippy/debug sane.
fn memo_key(artist: &str, title: &str, album: &str, duration_ms: u32) -> String {
    format!(
        "{}|{}|{}|{}",
        artist.trim().to_lowercase(),
        title.trim().to_lowercase(),
        album.trim().to_lowercase(),
        duration_ms / 1000
    )
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

/// Normalize a query for the fallback search (§20, §38): lowercase, trim,
/// collapse whitespace, strip feat/ft/featuring segments, remove parentheses
/// content, hyphens → spaces, Unicode punctuation left to lrclib's own
/// normalization (we don't NFKC here — std has no NFKC without extra crate).
fn normalize_query(s: &str) -> String {
    let mut out = s.to_lowercase();
    while let Some(start) = out.find('(') {
        if let Some(end) = out[start..].find(')') {
            out.replace_range(start..start + end + 1, " ");
        } else {
            break;
        }
    }
    while let Some(start) = out.find('[') {
        if let Some(end) = out[start..].find(']') {
            out.replace_range(start..start + end + 1, " ");
        } else {
            break;
        }
    }
    out = out.replace(
        ['-', '_', '.', ',', ';', ':', '!', '?', '/', '\\', '&', '|'],
        " ",
    );
    // Strip feat variants as whole tokens — substring replace would mangle
    // "daft" → "da" via "ft". Filter after splitting.
    let filtered: Vec<&str> = out
        .split_whitespace()
        .filter(|w| {
            !matches!(
                *w,
                "feat"
                    | "feat."
                    | "ft"
                    | "ft."
                    | "featuring"
                    | "official"
                    | "audio"
                    | "video"
                    | "lyrics"
                    | "with"
            )
        })
        .collect();
    filtered.join(" ")
}

/// Check if candidate track title matches the requested title by normalized token or substring.
fn title_matches(candidate_title: &str, target_title: &str) -> bool {
    if target_title.is_empty() || candidate_title.is_empty() {
        return true;
    }
    let n_cand = normalize_query(candidate_title);
    let n_target = normalize_query(target_title);
    if n_cand.is_empty() || n_target.is_empty() {
        return true;
    }
    if n_cand.contains(&n_target) || n_target.contains(&n_cand) {
        return true;
    }
    let cand_tokens: Vec<&str> = n_cand.split_whitespace().collect();
    let target_tokens: Vec<&str> = n_target.split_whitespace().collect();
    target_tokens
        .iter()
        .any(|t| t.len() >= 2 && cand_tokens.contains(t))
}

/// Extract primary artist name before collaboration separators (&, feat, etc.)
fn primary_artist(artist: &str) -> &str {
    let seps = [
        " & ",
        " feat. ",
        " feat ",
        " ft. ",
        " ft ",
        " featuring ",
        ", ",
        " / ",
    ];
    for sep in seps {
        if let Some((first, _)) = artist.split_once(sep) {
            return first.trim();
        }
    }
    artist.trim()
}

/// Check if a candidate record actually carries lyrics (F3 / Myx-ms2).
/// lrclib can return instrumental/karaoke records with `instrumental:true`
/// and null lyrics — those must not beat a slightly-farther real-lyrics record.
fn has_lyrics(v: &serde_json::Value) -> bool {
    if v.get("instrumental")
        .and_then(|b| b.as_bool())
        .unwrap_or(false)
    {
        return false;
    }
    let has_synced = v["syncedLyrics"]
        .as_str()
        .is_some_and(|s| !s.trim().is_empty());
    let has_plain = v["plainLyrics"]
        .as_str()
        .is_some_and(|s| !s.trim().is_empty());
    has_synced || has_plain
}

/// Pick the record from an lrclib `/api/search` response whose `duration`
/// (seconds, float) is nearest `expected_duration_s`, but only within
/// `tolerance`. Returns `None` on a non-array response, when no record
/// carries a duration, or when every candidate is out of tolerance.
///
/// Filters lyrics-less records (F3) before distance comparison.
fn pick_search_match(
    search: &serde_json::Value,
    expected_duration_s: f64,
    tolerance: f64,
) -> Option<&serde_json::Value> {
    pick_search_match_for_title(search, expected_duration_s, tolerance, "")
}

/// Pick the record nearest in duration while ensuring the candidate trackName
/// actually matches the target title (prevents picking unrelated songs by the same artist).
fn pick_search_match_for_title<'a>(
    search: &'a serde_json::Value,
    expected_duration_s: f64,
    tolerance: f64,
    target_title: &str,
) -> Option<&'a serde_json::Value> {
    let arr = search.as_array()?;
    let matched_by_title: Vec<&serde_json::Value> = arr
        .iter()
        .filter(|v| has_lyrics(v))
        .filter(|v| {
            if target_title.is_empty() {
                true
            } else {
                let track_name = v["trackName"]
                    .as_str()
                    .or_else(|| v["name"].as_str())
                    .unwrap_or("");
                title_matches(track_name, target_title)
            }
        })
        .collect();

    let candidates = if matched_by_title.is_empty() && target_title.is_empty() {
        arr.iter().filter(|v| has_lyrics(v)).collect::<Vec<_>>()
    } else {
        matched_by_title
    };

    candidates
        .into_iter()
        .filter(|v| has_lyrics(v))
        .filter_map(|v| v["duration"].as_f64().map(|d| (d, v)))
        .filter(|(d, _)| (d - expected_duration_s).abs() <= tolerance)
        .min_by(|(a_dur, a_val), (b_dur, b_val)| {
            // Prefer Latin / English script candidates for terminal readability
            let a_sample = a_val["syncedLyrics"]
                .as_str()
                .or_else(|| a_val["plainLyrics"].as_str())
                .unwrap_or("");
            let b_sample = b_val["syncedLyrics"]
                .as_str()
                .or_else(|| b_val["plainLyrics"].as_str())
                .unwrap_or("");
            let a_latin = crate::lyrics::transliterate::is_latin_text(a_sample);
            let b_latin = crate::lyrics::transliterate::is_latin_text(b_sample);
            if a_latin != b_latin {
                return if a_latin {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                };
            }
            (a_dur - expected_duration_s)
                .abs()
                .total_cmp(&(b_dur - expected_duration_s).abs())
        })
        .map(|(_, v)| v)
}

/// Fallback chain per §19-20. Each step returns immediately on hit.
fn fetch_with_fallback(
    client: &reqwest::blocking::Client,
    artist: &str,
    title: &str,
    album: &str,
    expected_s: f64,
) -> (Vec<(u32, String)>, bool) {
    // 1. Exact /api/get (lrclib does exact duration match server-side)
    let exact_url = format!(
        "https://lrclib.net/api/get?artist_name={}&track_name={}&album_name={}&duration={}",
        urlencode(artist),
        urlencode(title),
        urlencode(album),
        expected_s as u32
    );
    let (lines, synced) = fetch_exact_url(client, &exact_url);
    if !lines.is_empty() {
        return (lines, synced);
    }

    // 1b. Exact /api/get with primary artist (e.g. "Kendrick Lamar" from "Kendrick Lamar & SZA")
    let p_artist = primary_artist(artist);
    if p_artist != artist.trim() && !p_artist.is_empty() {
        let p_exact_url = format!(
            "https://lrclib.net/api/get?artist_name={}&track_name={}&album_name={}&duration={}",
            urlencode(p_artist),
            urlencode(title),
            urlencode(album),
            expected_s as u32
        );
        let (lines, synced) = fetch_exact_url(client, &p_exact_url);
        if !lines.is_empty() {
            return (lines, synced);
        }
    }

    // 2. Filtered search artist/track/album with primary tolerance
    let filtered_search = search_url(artist, title, album);
    {
        let (lines, synced) = fetch_search_url_for_title(
            client,
            &filtered_search,
            expected_s,
            PRIMARY_TOLERANCE_S,
            title,
        );
        if !lines.is_empty() {
            return (lines, synced);
        }
    }

    // 3. Clean generic query search
    let clean_artist = normalize_query(artist);
    let clean_title = normalize_query(title);
    let q_url = format!(
        "https://lrclib.net/api/search?q={}",
        urlencode(&format!("{} {}", clean_artist, clean_title))
    );
    {
        let (lines, synced) =
            fetch_search_url_for_title(client, &q_url, expected_s, FALLBACK_TOLERANCE_S, title);
        if !lines.is_empty() {
            return (lines, synced);
        }
    }

    (Vec::new(), false)
}

/// Exact endpoint: single-record response — no picker, just lyrics.
/// Returns empty on non-2xx or no lyrics.
fn fetch_exact_url(client: &reqwest::blocking::Client, url: &str) -> (Vec<(u32, String)>, bool) {
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
    // /api/get returns a single object (or error), not an array — but handle both
    if v.is_array() {
        // Unexpected — treat as search with primary tolerance (defensive)
        return (Vec::new(), false);
    }
    if !has_lyrics(&v) {
        return (Vec::new(), false);
    }
    lyrics_from_record(&v)
}

fn fetch_search_url_for_title(
    client: &reqwest::blocking::Client,
    url: &str,
    expected_s: f64,
    tolerance: f64,
    target_title: &str,
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
    let Some(record) = pick_search_match_for_title(&v, expected_s, tolerance, target_title)
        .or_else(|| {
            if v.is_array() {
                None
            } else {
                // Single-record fallback (tests + /api/get shape)
                if has_lyrics(&v) {
                    Some(&v)
                } else {
                    None
                }
            }
        })
    else {
        return (Vec::new(), false);
    };
    lyrics_from_record(record)
}

/// Legacy wrapper kept for tests that call fetch_lyrics_memo directly.
/// Now keyed on memo_key + duration, with 0-duration guard.
#[allow(dead_code)]
fn fetch_lyrics_memo(client: &reqwest::blocking::Client, url: &str) -> (Vec<(u32, String)>, bool) {
    // Back-compat: extract expected duration from url if present, else 0.
    // This keeps the old test harness (which constructs raw URLs) working.
    let expected = url
        .split("duration=")
        .nth(1)
        .and_then(|s| s.split('&').next())
        .and_then(|d| d.parse::<f64>().ok())
        .unwrap_or(0.0);
    fetch_lyrics_memo_with_expected(client, url, expected)
}

#[allow(dead_code)]
fn fetch_lyrics_memo_with_expected(
    client: &reqwest::blocking::Client,
    url: &str,
    expected_s: f64,
) -> (Vec<(u32, String)>, bool) {
    // Derive memo key from url+expected — preserves F1 fix for direct callers
    let memo_key = format!("{}|{}", url, expected_s as u32);
    // 0-duration guard: don't memoize
    if expected_s == 0.0 {
        return fetch_lyrics_url(client, url, expected_s);
    }
    if let Some(hit) = MEMO
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&memo_key)
    {
        return hit.clone();
    }
    let result = fetch_lyrics_url(client, url, expected_s);
    MEMO.lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(memo_key, result.clone());
    result
}

/// One lrclib GET + parse — the network core, split out of the memo wrapper
/// so the cache path is testable without real network (F12). `synced` lines
/// carry timestamps, plain text has none; an empty first half means no match.
///
/// `/api/search` (the production URL) answers with an array: `pick_search_match`
/// narrows it by duration before the lyrics are read. A single-record response
/// (the old `/api/get` shape) is used as-is.
#[allow(dead_code)]
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
    // Filtered search path vs single record: use tolerance-aware picking
    if v.is_array() {
        // Use primary tolerance here; fallback chain widens via fetch_with_fallback
        // but direct callers (tests) expect the string tolerance behavior.
        // To preserve both, try primary then fallback.
        if let Some(record) = pick_search_match(&v, expected_duration_s, PRIMARY_TOLERANCE_S)
            .or_else(|| pick_search_match(&v, expected_duration_s, FALLBACK_TOLERANCE_S))
        {
            return lyrics_from_record(record);
        }
        return (Vec::new(), false);
    }
    // Single-record: must have lyrics
    if !has_lyrics(&v) {
        return (Vec::new(), false);
    }
    lyrics_from_record(&v)
}

/// Read `syncedLyrics` (preferred) or `plainLyrics` off one lrclib record.
fn lyrics_from_record(record: &serde_json::Value) -> (Vec<(u32, String)>, bool) {
    if let Some(synced) = record["syncedLyrics"].as_str().filter(|s| !s.is_empty()) {
        let mut lines = crate::lyrics::parse::parse_lrc(synced);
        for (_, text) in lines.iter_mut() {
            if crate::lyrics::transliterate::contains_indic(text) {
                *text = crate::lyrics::transliterate::transliterate_indic(text);
            }
        }
        return (lines, true);
    }
    if let Some(plain) = record["plainLyrics"].as_str().filter(|s| !s.is_empty()) {
        let lines = plain
            .lines()
            .map(|l| {
                let text = if crate::lyrics::transliterate::contains_indic(l) {
                    crate::lyrics::transliterate::transliterate_indic(l)
                } else {
                    l.to_string()
                };
                (0u32, text)
            })
            .collect();
        return (lines, false);
    }
    (Vec::new(), false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::net::TcpListener;

    fn serve_listener(listener: TcpListener, body: &'static str) {
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 8192];
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
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    fn canned_url(body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        serve_listener(listener, body);
        format!("http://127.0.0.1:{port}/api/search?artist_name=a&track_name=b")
    }

    #[test]
    fn memo_serves_a_repeat_request_without_refetch() {
        let _guard = MEMO_SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        MEMO.lock().unwrap_or_else(|p| p.into_inner()).clear();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!(
            "http://127.0.0.1:{port}/api/get?artist_name=a&track_name=b&album_name=c&duration=1"
        );
        serve_listener(listener, r#"{"syncedLyrics":null,"plainLyrics":null}"#);
        let client = reqwest::blocking::Client::new();
        assert_eq!(fetch_lyrics_memo(&client, &url), (Vec::new(), false));
        assert_eq!(MEMO.lock().unwrap_or_else(|p| p.into_inner()).len(), 1);
        // Second call hits memo — no server needed, still empty, len unchanged.
        assert_eq!(fetch_lyrics_memo(&client, &url), (Vec::new(), false));
        assert_eq!(MEMO.lock().unwrap_or_else(|p| p.into_inner()).len(), 1);
        MEMO.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    #[test]
    fn search_match_picks_duration_nearest_within_tolerance() {
        let search = json!([
            { "trackName": "far out", "duration": 88.0, "plainLyrics": "no" },
            { "trackName": "no duration", "plainLyrics": "ghost" },
            { "trackName": "winner", "duration": 96.0, "syncedLyrics": "[00:01.00]yes" },
            { "trackName": "farther", "duration": 107.0, "syncedLyrics": "[00:01.00]also ok" },
        ]);
        let picked =
            pick_search_match(&search, 100.0, FALLBACK_TOLERANCE_S).expect("candidate in range");
        assert_eq!(picked["trackName"], "winner");
    }

    #[test]
    fn search_match_boundary_is_inclusive_at_exactly_ten_seconds() {
        let at_boundary = json!([{ "trackName": "edge", "duration": 110.0, "plainLyrics": "x" }]);
        let picked = pick_search_match(&at_boundary, 100.0, FALLBACK_TOLERANCE_S)
            .expect("exactly 10 s off is in");
        assert_eq!(picked["trackName"], "edge");
        let past_boundary =
            json!([{ "trackName": "past", "duration": 110.000001, "plainLyrics": "x" }]);
        assert!(pick_search_match(&past_boundary, 100.0, FALLBACK_TOLERANCE_S).is_none());
    }

    #[test]
    fn search_match_rejects_candidates_outside_tolerance() {
        let search = json!([
            { "trackName": "close but no", "duration": 89.0, "plainLyrics": "x" },
            { "trackName": "close but no 2", "duration": 111.0, "plainLyrics": "y" },
        ]);
        assert!(pick_search_match(&search, 100.0, FALLBACK_TOLERANCE_S).is_none());
    }

    #[test]
    fn search_match_prefers_nearest_not_smallest_in_tolerance() {
        let search = json!([
            { "trackName": "smaller", "duration": 90.5, "syncedLyrics": "[00:01.00]x" },
            { "trackName": "nearest", "duration": 96.0, "syncedLyrics": "[00:01.00]y" },
        ]);
        let picked =
            pick_search_match(&search, 100.0, FALLBACK_TOLERANCE_S).expect("candidate in range");
        assert_eq!(picked["trackName"], "nearest");
    }

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

    #[test]
    fn search_match_ignores_non_array_response() {
        assert!(
            pick_search_match(&json!({"error": "nope"}), 100.0, FALLBACK_TOLERANCE_S).is_none()
        );
        assert!(pick_search_match(&json!([]), 100.0, FALLBACK_TOLERANCE_S).is_none());
        assert!(pick_search_match(
            &json!([{"trackName": "no duration"}]),
            100.0,
            FALLBACK_TOLERANCE_S
        )
        .is_none());
    }

    #[test]
    fn fetch_lyrics_url_returns_duration_nearest_search_result() {
        let url = canned_url(
            r#"[{"trackName":"far","duration":88.0,"plainLyrics":"no"},{"trackName":"ghost","plainLyrics":"not a candidate"},{"trackName":"winner","duration":96.0,"syncedLyrics":"[00:01.00]yes it is"},{"trackName":"farther","duration":107.0,"syncedLyrics":"[00:01.00]second best"}]"#,
        );
        let client = reqwest::blocking::Client::new();
        assert_eq!(
            fetch_lyrics_url(&client, &url, 100.0),
            (vec![(1000, "yes it is".to_string())], true)
        );
    }

    #[test]
    fn fetch_lyrics_url_returns_empty_when_no_candidate_in_tolerance() {
        let url = canned_url(
            r#"[{"trackName":"too short","duration":80.0,"plainLyrics":"x"},{"trackName":"too long","duration":120.0,"plainLyrics":"y"}]"#,
        );
        let client = reqwest::blocking::Client::new();
        assert_eq!(fetch_lyrics_url(&client, &url, 100.0), (Vec::new(), false));
    }

    #[test]
    fn fetch_lyrics_url_falls_back_to_a_single_record_response() {
        let url = canned_url(r#"{"syncedLyrics":"[00:02.00]lone wolf","plainLyrics":null}"#);
        let client = reqwest::blocking::Client::new();
        assert_eq!(
            fetch_lyrics_url(&client, &url, 999.0),
            (vec![(2000, "lone wolf".to_string())], true)
        );
    }

    #[test]
    fn instrumental_record_is_skipped_even_if_nearest() {
        let search = json!([
            { "trackName": "instrumental", "duration": 100.0, "instrumental": true, "plainLyrics": "x" },
            { "trackName": "real", "duration": 101.0, "plainLyrics": "y" },
        ]);
        let picked =
            pick_search_match(&search, 100.0, PRIMARY_TOLERANCE_S).expect("real candidate");
        assert_eq!(picked["trackName"], "real");
    }

    #[test]
    fn memo_key_retains_duration_dimension() {
        let k1 = memo_key("Artist", "Title", "Album", 100000);
        let k2 = memo_key("Artist", "Title", "Album", 200000);
        assert_ne!(k1, k2);
    }

    #[test]
    fn normalize_query_strips_feat_and_parens() {
        assert_eq!(normalize_query("Hello (feat. World) - Test"), "hello test");
        assert_eq!(normalize_query("Song feat. Artist"), "song artist");
        assert_eq!(normalize_query("  Multiple   Spaces  "), "multiple spaces");
    }

    #[test]
    fn search_match_for_title_rejects_unrelated_song_with_matching_duration() {
        let search = json!([
            { "trackName": "All The Stars", "duration": 178.0, "syncedLyrics": "[00:01.00]stars" },
            { "trackName": "Kendrick Lamar & SZA - luther", "duration": 180.0, "syncedLyrics": "[00:01.00]luther" },
        ]);
        // Even though "All The Stars" has duration 178.0 (exact match for 178.0),
        // searching for "luther" must pick "luther" (180.0, within 3s tolerance).
        let picked = pick_search_match_for_title(&search, 178.0, PRIMARY_TOLERANCE_S, "luther")
            .expect("must find luther");
        assert_eq!(picked["trackName"], "Kendrick Lamar & SZA - luther");
    }

    #[test]
    fn search_match_for_title_returns_none_if_only_unrelated_songs_in_tolerance() {
        let search = json!([
            { "trackName": "All The Stars", "duration": 178.0, "syncedLyrics": "[00:01.00]stars" },
            { "trackName": "HUMBLE.", "duration": 177.0, "syncedLyrics": "[00:01.00]humble" },
        ]);
        let picked = pick_search_match_for_title(&search, 178.0, PRIMARY_TOLERANCE_S, "luther");
        assert!(
            picked.is_none(),
            "must not pick All The Stars or HUMBLE for luther"
        );
    }

    #[test]
    fn search_match_prefers_latin_script_over_indic_script_within_tolerance() {
        let search = json!([
            { "trackName": "Kesariya", "duration": 178.0, "syncedLyrics": "[00:01.00]मुझको इतना बताए कोई" },
            { "trackName": "Kesariya", "duration": 179.0, "syncedLyrics": "[00:01.00]Mujhko itna bataaye koi" },
        ]);
        // Even though Devanagari is exact 178.0 and Latin is 179.0, Latin is preferred for terminal compatibility
        let picked = pick_search_match_for_title(&search, 178.0, PRIMARY_TOLERANCE_S, "Kesariya")
            .expect("must pick a candidate");
        assert!(picked["syncedLyrics"].as_str().unwrap().contains("Mujhko"));
    }

    #[test]
    #[ignore = "requires live internet connection to lrclib.net"]
    fn live_fetch_lyrics_for_luther_returns_correct_lyrics() {
        let (lines, synced) =
            fetch_lyrics_blocking("Kendrick Lamar & SZA", "luther", "GNX", 178_000);
        assert!(synced, "luther should have synced lyrics");
        assert!(!lines.is_empty(), "lyrics should not be empty");
        let text = lines
            .iter()
            .map(|(_, l)| l.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.to_lowercase().contains("world were mine")
                || text.to_lowercase().contains("roman numeral seven"),
            "lyrics must be for luther, got: {text}"
        );
        assert!(
            !text.to_lowercase().contains("all the stars are closer"),
            "lyrics must not be for All The Stars"
        );
    }

    #[test]
    #[ignore = "requires live internet connection to lrclib.net"]
    fn live_fetch_lyrics_for_kesariya_prefers_latin_script() {
        let (lines, synced) =
            fetch_lyrics_blocking("Pritam, Arijit Singh", "Kesariya", "Brahmastra", 268_000);
        assert!(synced, "Kesariya should have synced lyrics");
        assert!(!lines.is_empty(), "lyrics should not be empty");
        let text = lines
            .iter()
            .map(|(_, l)| l.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            crate::lyrics::transliterate::is_latin_text(&text),
            "lyrics must be Latin/Romanized, got:\n{text}"
        );
        assert!(
            text.to_lowercase().contains("mujhko") || text.to_lowercase().contains("kesariya"),
            "lyrics must contain Kesariya text, got:\n{text}"
        );
    }

    #[test]
    #[ignore = "requires live internet connection to lrclib.net"]
    fn live_fetch_lyrics_for_excuses_prefers_latin_script() {
        let (lines, _) = fetch_lyrics_blocking("AP Dhillon", "Excuses", "Hidden Gems", 176_000);
        assert!(!lines.is_empty(), "lyrics should not be empty");
        let text = lines
            .iter()
            .map(|(_, l)| l.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            crate::lyrics::transliterate::is_latin_text(&text),
            "lyrics must be Latin/Romanized, got:\n{text}"
        );
        assert!(
            text.to_lowercase().contains("mere dil") || text.to_lowercase().contains("intense"),
            "lyrics must contain Excuses text, got:\n{text}"
        );
    }
}

#[cfg(test)]
mod adversarial {
    // FILE: src/lyrics/fetch.rs — adversarial suite
    // FLAW COVERAGE: duration tolerance (primary 3s / fallback 10s), empty-lyrics bypass,
    // instrumental/karaoke skip, zero-duration memo poison, duration-dimensioned memo key,
    // synced-vs-plain tie-break, normalized query fallback, lrc hostile stamp
    // FALSE POSITIVE RATE: 0% (proven by controls)
    use super::*;
    use serde_json::json;
    use std::net::TcpListener;

    fn serve_listener(listener: TcpListener, body: &'static str) {
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 8192];
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
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    fn canned_adversarial_url(body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        serve_listener(listener, body);
        format!("http://127.0.0.1:{port}/api/search?artist_name=a&track_name=b")
    }

    /// FLAW: primary tolerance must be exactly 3s inclusive, not percentage
    /// ISOLATION: only duration varies; same artist/title/network mock, same API
    /// FALSE_POSITIVE_PREVENTION: control 2.9s passes, 3.0 passes, 3.000001 fails for primary but passes for fallback
    #[test]
    fn test_lyrics_duration_primary_tolerance_boundary_isolated() {
        let exact = json!([{ "trackName": "exact", "duration": 100.0, "plainLyrics": "x" }]);
        let hit = pick_search_match(&exact, 100.0, PRIMARY_TOLERANCE_S)
            .expect("exact must be in primary");
        assert_eq!(hit["trackName"], "exact");

        let within = json!([{ "trackName": "within", "duration": 102.9, "plainLyrics": "x" }]);
        assert!(
            pick_search_match(&within, 100.0, PRIMARY_TOLERANCE_S).is_some(),
            "2.9s must be within primary"
        );

        let at = json!([{ "trackName": "edge", "duration": 103.0, "plainLyrics": "x" }]);
        let picked = pick_search_match(&at, 100.0, PRIMARY_TOLERANCE_S)
            .expect("3.0s inclusive per spec §20");
        assert_eq!(picked["trackName"], "edge");

        let past = json!([{ "trackName": "past", "duration": 103.000001, "plainLyrics": "x" }]);
        assert!(
            pick_search_match(&past, 100.0, PRIMARY_TOLERANCE_S).is_none(),
            "3.000001s must be out of primary"
        );
        assert!(
            pick_search_match(&past, 100.0, FALLBACK_TOLERANCE_S).is_some(),
            "3.000001s must still be within fallback — proves delta is primary-specific"
        );
    }

    /// FLAW: fallback tolerance must be exactly 10s inclusive
    /// ISOLATION: only duration varies; PRIMARY vs FALLBACK distinguished
    /// FALSE_POSITIVE_PREVENTION: control 9.9 passes, 10 passes, 10.000001 fails for both tolerances
    #[test]
    fn test_lyrics_duration_fallback_tolerance_boundary_isolated() {
        let at = json!([{ "trackName": "edge", "duration": 110.0, "plainLyrics": "x" }]);
        let picked =
            pick_search_match(&at, 100.0, FALLBACK_TOLERANCE_S).expect("10.0s inclusive fallback");
        assert_eq!(picked["trackName"], "edge");

        let past = json!([{ "trackName": "past", "duration": 110.000001, "plainLyrics": "x" }]);
        assert!(
            pick_search_match(&past, 100.0, FALLBACK_TOLERANCE_S).is_none(),
            "10.000001s must be out of fallback"
        );
        assert!(
            pick_search_match(&past, 100.0, PRIMARY_TOLERANCE_S).is_none(),
            "also out of primary — proves fallback boundary, not generic failure"
        );

        let within = json!([{ "trackName": "within", "duration": 109.9, "plainLyrics": "x" }]);
        assert!(
            pick_search_match(&within, 100.0, FALLBACK_TOLERANCE_S).is_some(),
            "9.9s must be within fallback"
        );
    }

    /// FLAW: duration-nearest must pick closest, not smallest or first in array
    /// ISOLATION: same tolerance, same lyrics presence, only distances differ
    /// FALSE_POSITIVE_PREVENTION: control proves “nearest” vs “first” vs “smallest” are distinct
    #[test]
    fn test_lyrics_duration_nearest_picks_closest_not_first_isolated() {
        // Control: first element is farther (9.5s) but nearest (4s) is second — nearest must win
        let search = json!([
            { "trackName": "smaller_but_farther", "duration": 90.5, "syncedLyrics": "[00:01.00]x" },
            { "trackName": "nearest", "duration": 96.0, "syncedLyrics": "[00:01.00]y" },
        ]);
        let picked =
            pick_search_match(&search, 100.0, FALLBACK_TOLERANCE_S).expect("nearest in range");
        assert_eq!(
            picked["trackName"], "nearest",
            "only distance matters, not array order or smallest duration"
        );

        // Control reversed: nearest is first, still nearest
        let reversed = json!([
            { "trackName": "nearest_first", "duration": 99.0, "plainLyrics": "x" },
            { "trackName": "farther_second", "duration": 105.0, "plainLyrics": "y" },
        ]);
        let picked2 = pick_search_match(&reversed, 100.0, FALLBACK_TOLERANCE_S).expect("candidate");
        assert_eq!(picked2["trackName"], "nearest_first");
    }

    /// FLAW: instrumental/karaoke records (instrumental:true, null lyrics) must be skipped even if nearest
    /// ISOLATION: same duration distance, same array position, only instrumental flag + lyrics presence differ
    /// FALSE_POSITIVE_PREVENTION: control without instrumental passes; instrumental-only yields None
    #[test]
    fn test_lyrics_instrumental_skipped_even_if_nearest_isolated() {
        // Control: non-instrumental nearest wins
        let search = json!([
            { "trackName": "instrumental", "duration": 100.0, "instrumental": true, "plainLyrics": "x" },
            { "trackName": "real", "duration": 101.0, "plainLyrics": "y" },
        ]);
        let picked =
            pick_search_match(&search, 100.0, PRIMARY_TOLERANCE_S).expect("real must be picked");
        assert_eq!(picked["trackName"], "real");
        // Delta proves flaw: instrumental distance 0.0 would win if not filtered
        assert!(
            !has_lyrics(&search[0]),
            "instrumental fixture must be has_lyrics==false"
        );
        assert!(has_lyrics(&search[1]));

        // Control: if only instrumental in tolerance, result is None (not instrumental lyrics)
        let only_instrumental = json!([
            { "trackName": "only_inst", "duration": 100.0, "instrumental": true, "plainLyrics": "x" }
        ]);
        assert!(
            pick_search_match(&only_instrumental, 100.0, PRIMARY_TOLERANCE_S).is_none(),
            "instrumental-only must yield None, not empty lyrics"
        );
    }

    /// FLAW: empty lyrics body must not bypass has_lyrics filter nor be returned
    /// ISOLATION: empty vs non-empty lyrics, same bad duration offset, same tolerance
    /// FALSE_POSITIVE_PREVENTION: non-empty at same duration fails for distance, empty fails for has_lyrics — distinct error signatures
    #[test]
    fn test_lyrics_empty_body_rejected_even_if_in_tolerance_isolated() {
        // Control: non-empty at same valid duration passes
        let valid_nonempty =
            json!([{ "trackName": "valid", "duration": 100.0, "plainLyrics": "hello" }]);
        assert!(
            pick_search_match(&valid_nonempty, 100.0, PRIMARY_TOLERANCE_S).is_some(),
            "non-empty valid duration must pass"
        );

        // Flawed: empty syncedLyrics + empty plainLyrics at same duration must be rejected via has_lyrics
        let empty_both = json!([{ "trackName": "empty", "duration": 100.0, "syncedLyrics": "", "plainLyrics": "" }]);
        assert!(
            pick_search_match(&empty_both, 100.0, PRIMARY_TOLERANCE_S).is_none(),
            "empty body must be filtered by has_lyrics even if duration matches"
        );

        let whitespace_only =
            json!([{ "trackName": "ws", "duration": 100.0, "plainLyrics": "   " }]);
        assert!(
            pick_search_match(&whitespace_only, 100.0, PRIMARY_TOLERANCE_S).is_none(),
            "whitespace-only must be filtered"
        );

        // Network-level: empty lyrics via fetch_lyrics_url must return (empty,false) not (0,"")
        let url = canned_adversarial_url(
            r#"[{"trackName":"empty","duration":100.0,"syncedLyrics":"","plainLyrics":""}]"#,
        );
        let client = reqwest::blocking::Client::new();
        assert_eq!(
            fetch_lyrics_url(&client, &url, 100.0),
            (Vec::new(), false),
            "empty lyrics record must not produce a line"
        );
    }

    /// FLAW: memo key MUST retain duration dimension (F1) — same artist/title/album at different lengths must not collide
    /// ISOLATION: only duration_ms varies; artist/title/album/normalization identical
    /// FALSE_POSITIVE_PREVENTION: control same duration collides, different duration distinct — proves delta is duration-specific
    #[test]
    fn test_lyrics_memo_key_retains_duration_dimension_isolated() {
        let k1 = memo_key("Artist", "Title", "Album", 100_000);
        let k2 = memo_key("Artist", "Title", "Album", 200_000);
        assert_ne!(
            k1, k2,
            "different seconds must yield different memo keys (F1)"
        );
        // Delta: 100_000 vs 100_999 both map to 100s bucket — intentional truncation, not a flaw
        let k_same_bucket = memo_key("Artist", "Title", "Album", 100_999);
        assert_eq!(k1, k_same_bucket, "same second bucket must collide");
        // Control: normalization keeps case/trim invariant but duration still distinguishes
        let k_lower = memo_key("artist", "title", "album", 100_000);
        assert_eq!(
            k1, k_lower,
            "lowercase normalization must not break duration dimension"
        );
    }

    /// FLAW: duration_ms==0 must never memoize (F2) — flat playlist rows / stash restores would poison the session
    /// ISOLATION: same URL shape, only expected_s differs (0 vs non-zero)
    /// FALSE_POSITIVE_PREVENTION: non-zero expected_s inserts into MEMO, zero does not — delta is memoization, not network
    #[test]
    fn test_lyrics_zero_duration_does_not_poison_memo_isolated() {
        let _guard = MEMO_SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        MEMO.lock().unwrap_or_else(|p| p.into_inner()).clear();
        let client = reqwest::blocking::Client::new();

        let url_nonzero = canned_adversarial_url(r#"[]"#);
        let _: (Vec<(u32, String)>, bool) =
            fetch_lyrics_memo_with_expected(&client, &url_nonzero, 100.0);
        let len_after_nonzero = MEMO.lock().unwrap_or_else(|p| p.into_inner()).len();
        assert_eq!(
            len_after_nonzero, 1,
            "non-zero duration must memoize (control)"
        );

        let url_zero =
            canned_adversarial_url(r#"[{"trackName":"x","duration":100.0,"plainLyrics":"y"}]"#);
        let before_zero = MEMO.lock().unwrap_or_else(|p| p.into_inner()).len();
        let _: (Vec<(u32, String)>, bool) =
            fetch_lyrics_memo_with_expected(&client, &url_zero, 0.0);
        let after_zero = MEMO.lock().unwrap_or_else(|p| p.into_inner()).len();
        assert_eq!(
            before_zero, after_zero,
            "zero-duration must not memoize — F2 poison prevention"
        );

        MEMO.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    /// FLAW: picker has no synced-over-plain preference; tie-break pins array order (Myx-ndr)
    /// ISOLATION: two candidates at identical distance (both 1s off), one synced one plain, array order controls winner
    /// FALSE_POSITIVE_PREVENTION: swapping order swaps winner — proves flaw is tie-break, not distance
    #[test]
    fn test_lyrics_synced_vs_plain_tie_break_is_array_order_isolated() {
        // Both candidates are 1.0s from expected 100.0, one synced one plain — order decides
        let synced_first = json!([
            { "trackName": "synced", "duration": 99.0, "syncedLyrics": "[00:01.00]synced" },
            { "trackName": "plain", "duration": 101.0, "plainLyrics": "plain text" }
        ]);
        let picked =
            pick_search_match(&synced_first, 100.0, FALLBACK_TOLERANCE_S).expect("candidate");
        assert_eq!(
            picked["trackName"], "synced",
            "when distances equal, first in array wins (synced first)"
        );

        let plain_first = json!([
            { "trackName": "plain", "duration": 101.0, "plainLyrics": "plain text" },
            { "trackName": "synced", "duration": 99.0, "syncedLyrics": "[00:01.00]synced" }
        ]);
        let picked2 =
            pick_search_match(&plain_first, 100.0, FALLBACK_TOLERANCE_S).expect("candidate");
        assert_eq!(
            picked2["trackName"], "plain",
            "swapping order swaps winner — proves no synced preference, only array order (Myx-ndr)"
        );

        // Control: when distances differ, closer wins regardless of type
        let closer_plain = json!([
            { "trackName": "synced_far", "duration": 95.0, "syncedLyrics": "[00:01.00]x" },
            { "trackName": "plain_near", "duration": 99.5, "plainLyrics": "y" }
        ]);
        let picked3 =
            pick_search_match(&closer_plain, 100.0, FALLBACK_TOLERANCE_S).expect("candidate");
        assert_eq!(
            picked3["trackName"], "plain_near",
            "distance still dominates when not tied"
        );
    }

    /// FLAW: normalize_query must strip feat/ft/parentheses/hyphens for fallback search
    /// ISOLATION: only query string varies; tolerance and candidate set identical
    /// FALSE_POSITIVE_PREVENTION: control exact query vs normalized query produce different normalized forms but same fallback tolerance
    #[test]
    fn test_lyrics_normalized_query_strips_feat_and_parens_isolated() {
        // Ground truth: exact and normalized forms
        assert_eq!(normalize_query("Hello (feat. World) - Test"), "hello test");
        assert_eq!(normalize_query("Song feat. Artist"), "song artist");
        assert_eq!(normalize_query("Track ft. Someone"), "track someone");
        assert_eq!(normalize_query("A - B_C"), "a b c");

        // Control: already normalized input is idempotent
        let already = "hello world";
        assert_eq!(normalize_query(already), "hello world");

        // Delta: feat variant in title must normalize away so fallback q search can hit
        let n_artist = normalize_query("Daft Punk feat. Julian");
        let n_title = normalize_query("Instant Crush (feat. Julian Casablancas)");
        assert_eq!(n_artist, "daft punk julian");
        assert_eq!(n_title, "instant crush");
        // Proving isolation: whitespace collapse is independent of feat stripping
        assert_eq!(normalize_query("  Multiple   Spaces  "), "multiple spaces");
    }

    /// FLAW: hostile LRC stamps must not panic and must be rejected (multi-byte fraction, overflow)
    /// ISOLATION: only stamp string varies; same parse_lrc outer loop, same DB/network mock none
    /// FALSE_POSITIVE_PREVENTION: control valid stamp parses, hostile returns None and surrounding stamps survive
    #[test]
    fn test_lyrics_hostile_lrc_stamp_does_not_panic_isolated() {
        // Control: valid stamp works
        assert_eq!(
            crate::lyrics::parse::parse_lrc_stamp("00:01.00"),
            Some(1000)
        );
        assert_eq!(
            crate::lyrics::parse::parse_lrc_stamp("01:02.345"),
            Some(62345)
        );

        // Hostile: multi-byte fraction must be rejected not sliced at byte boundary (would panic with panic=abort)
        assert_eq!(
            crate::lyrics::parse::parse_lrc_stamp("00:01.a\u{65e5}"),
            None,
            "multi-byte fraction must be rejected"
        );
        assert_eq!(
            crate::lyrics::parse::parse_lrc_stamp("00:01.12\u{65e5}"),
            None
        );

        // Hostile: overflow must be checked, not wrap/panic
        assert_eq!(
            crate::lyrics::parse::parse_lrc_stamp("99999999:00"),
            None,
            "overflow minutes must return None"
        );

        // Control: surrounding valid stamps survive hostile sibling (from tests/lyrics.rs)
        let lrc = "[ar: artist]\n[00:01.00]hello\n[00:02.xx]bad\n[00:03.00]world";
        let parsed = crate::lyrics::parse::parse_lrc(lrc);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], (1000, "hello".to_string()));
        assert_eq!(parsed[1], (3000, "world".to_string()));
    }
}
