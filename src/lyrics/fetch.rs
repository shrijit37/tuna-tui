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
