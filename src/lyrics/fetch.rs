//! Lyric fetching, from lrclib.net — the only network-dependent lyrics source.
//!
//! Keyed on artist/title/album/duration rather than a provider track id, so it
//! survived the Spotify→YouTube port untouched: only the metadata source above
//! changed, and the key fields (`yt:` video title/channel/duration) feed the
//! same query. This is the `src/api/lyrics.rs` body, relocated into the library
//! so it outlives the bin-side api layer.

use std::sync::OnceLock;

use crate::util::urlencode;

/// One client for the whole process: lrclib fetches are rare (one per track
/// change) but each used to build a fresh client — TLS setup + connection
/// pool per request. `reqwest::blocking::Client` is `Send + Sync`, so a
/// shared instance is sound across the worker threads.
static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();

/// Fetch lyrics for a track from lrclib. Returns `(lines, synced)`; synced
/// lines carry timestamps, plain text has none. An empty first half means no
/// match — the caller renders the view empty.
pub fn fetch_lyrics_blocking(
    artist: &str,
    title: &str,
    album: &str,
    duration_ms: u32,
) -> (Vec<(u32, String)>, bool) {
    let client = CLIENT.get_or_init(|| crate::httpcache::blocking_client().clone());
    let url = format!(
        "https://lrclib.net/api/get?artist_name={}&track_name={}&album_name={}&duration={}",
        urlencode(artist),
        urlencode(title),
        urlencode(album),
        duration_ms / 1000
    );
    let Ok(resp) = client
        .get(&url)
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

    if let Some(synced) = v["syncedLyrics"].as_str().filter(|s| !s.is_empty()) {
        return (crate::lyrics::parse::parse_lrc(synced), true);
    }
    if let Some(plain) = v["plainLyrics"].as_str().filter(|s| !s.is_empty()) {
        let lines = plain.lines().map(|l| (0u32, l.to_string())).collect();
        return (lines, false);
    }
    (Vec::new(), false)
}
