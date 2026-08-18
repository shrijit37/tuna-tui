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
    fetch_lyrics_memo(client, &url)
}

/// The memo wrapper: identical requests (same URL) are served from memory —
/// the network legs of [`fetch_lyrics_url`] never run twice for one track in
/// one session. The client is injected so tests can point the miss path at an
/// offline endpoint without touching the real lrclib.net. Never holds the
/// lock across the network fetch (a memo miss must not serialize fetches).
fn fetch_lyrics_memo(client: &reqwest::blocking::Client, url: &str) -> (Vec<(u32, String)>, bool) {
    if let Some(hit) = MEMO.lock().unwrap_or_else(|p| p.into_inner()).get(url) {
        return hit.clone();
    }
    let result = fetch_lyrics_url(client, url);
    MEMO.lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(url.to_string(), result.clone());
    result
}

/// One lrclib GET + parse — the network core, split out of the memo wrapper
/// so the cache path is testable without real network (F12). `synced` lines
/// carry timestamps, plain text has none; an empty first half means no match.
fn fetch_lyrics_url(client: &reqwest::blocking::Client, url: &str) -> (Vec<(u32, String)>, bool) {
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

    if let Some(synced) = v["syncedLyrics"].as_str().filter(|s| !s.is_empty()) {
        return (crate::lyrics::parse::parse_lrc(synced), true);
    }
    if let Some(plain) = v["plainLyrics"].as_str().filter(|s| !s.is_empty()) {
        let lines = plain.lines().map(|l| (0u32, l.to_string())).collect();
        return (lines, false);
    }
    (Vec::new(), false)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(fetch_lyrics_memo(&client, &url), (Vec::new(), false));
        assert_eq!(MEMO.lock().unwrap().len(), 1);

        // Serve real lyrics on the very same URL now.
        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf);
                let body = r#"{"syncedLyrics":"[00:01.00]hello there","plainLyrics":null}"#;
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

        // Call 2 — identical args: the memo returns the cached miss and never
        // touches the (now live) server; a real fetch would return lyrics.
        assert_eq!(fetch_lyrics_memo(&client, &url), (Vec::new(), false));
        assert_eq!(MEMO.lock().unwrap().len(), 1);
    }
}
