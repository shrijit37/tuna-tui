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
