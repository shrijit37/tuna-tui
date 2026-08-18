//! Spotify metadata for quality enrichment — quality over gray zone.
//!
//! Primary leg: the standard **client-credentials** OAuth flow against
//! `accounts.spotify.com/api/token`, driven by the `SPOTIFY_CLIENT_ID` /
//! `SPOTIFY_CLIENT_SECRET` env vars (app-level credentials from the free
//! Spotify developer dashboard — the same model spotDL uses; nothing
//! user-account related, and nothing in the repo's config files). The
//! credentials token is cached until it expires (usually an hour).
//!
//! Fallback leg (probe-verified DEAD on this box on 2026-08-19): the
//! anonymous web-player token (`open.spotify.com/get_access_token`) of the
//! librespot/spotify-tui lineage answered with an HTML error page, and the
//! `clienttoken.spotify.com` leg answers 400/405. It stays as a
//! no-credentials fallback for the day the door reopens; it is never
//! depended on.
//!
//! Every consumer treats a miss as "keep the YouTube-derived metadata":
//! `search_track` returns `None` on any failure and the engine falls back
//! silently — the feature degrades, it never breaks playback.
//!
//! The pattern: the anonymous web-player token (`open.spotify.com/
//! get_access_token`) that the librespot/spotify-tui lineage has used for
//! years — spotDL now routes through provider credentials, which this port
//! deliberately has no place for (the Spotify API layer + OAuth were
//! deleted with the port). The anonymous token is scoped to metadata
//! lookup, not playback, and every consumer treats a miss as "keep the
//! YouTube-derived metadata".

use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::liblog::liblog;

/// One canonical track hit from the Spotify search API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpotifyTrack {
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    pub duration_ms: u32,
    pub image_url: Option<String>,
    pub isrc: Option<String>,
}

const TOKEN_URL: &str =
    "https://open.spotify.com/get_access_token?reason=transport&productType=web_player";
const SEARCH_URL: &str = "https://api.spotify.com/v1/search";
/// The endpoint family expects a browser-ish user agent; without one the
/// anonymous token route answers with a bot-gate. Same trick the yt-dlp
/// client does on its own legs.
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36";

/// Token cache keyed by which leg produced it: the credentials token
/// expires in ~1h and its fetch is the slowest leg; one per process is
/// plenty.
fn token_cache() -> &'static Mutex<Option<(String, Instant)>> {
    static CACHE: OnceLock<Mutex<Option<(String, Instant)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

const CREDS_URL: &str = "https://accounts.spotify.com/api/token";

/// The primary leg: `client_credentials` grant with app credentials from
/// the environment. Standard OAuth form-post; returns the access token.
fn fetch_creds_token(client: &reqwest::blocking::Client) -> Option<String> {
    let id = std::env::var("SPOTIFY_CLIENT_ID").ok()?;
    let secret = std::env::var("SPOTIFY_CLIENT_SECRET").ok()?;
    // reqwest 0.13's blocking builder has no `.form()` — the standard
    // form-post rides as an explicit body + content-type.
    let resp = client
        .post(CREDS_URL)
        .basic_auth(&id, Some(&secret))
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body("grant_type=client_credentials".to_string())
        .timeout(Duration::from_secs(6))
        .send()
        .ok()?;
    if !resp.status().is_success() {
        liblog(format!(
            "spotify: credentials -> HTTP {}",
            resp.status().as_u16()
        ));
        return None;
    }
    let body: serde_json::Value = resp.json().ok()?;
    let token = body
        .get("access_token")
        .and_then(|v| v.as_str())?
        .to_string();
    if token.is_empty() {
        return None;
    }
    *token_cache().lock().unwrap() = Some((token.clone(), Instant::now()));
    Some(token)
}

/// Fallback leg, probe-verified dead on this box (2026-08-19): the
/// anonymous web-player token answers an HTML error page. Kept as a
/// no-credentials fallback for when it reopens.
fn fetch_anon_token(client: &reqwest::blocking::Client) -> Option<String> {
    {
        let cache = token_cache().lock().unwrap();
        if let Some((tok, at)) = cache.as_ref() {
            if at.elapsed() < Duration::from_secs(60 * 50) {
                return Some(tok.clone());
            }
        }
    }
    let resp = client
        .get(TOKEN_URL)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .timeout(Duration::from_secs(6))
        .send()
        .ok()?;
    if !resp.status().is_success() {
        liblog(format!("spotify: token -> HTTP {}", resp.status().as_u16()));
        return None;
    }
    let body: serde_json::Value = resp.json().ok()?;
    let token = body
        .get("accessToken")
        .and_then(|v| v.as_str())?
        .to_string();
    if token.is_empty() {
        return None;
    }
    *token_cache().lock().unwrap() = Some((token.clone(), Instant::now()));
    Some(token)
}

/// Search Spotify for the best `type=track` hit for `query`. A miss (no
/// hit, network error, bot-gate, whatever) is `None` — the caller keeps its
/// own metadata.
pub fn search_track(client: &reqwest::blocking::Client, query: &str) -> Option<SpotifyTrack> {
    if query.trim().is_empty() {
        return None;
    }

    /// Credentials first, anonymous fallback — both cached, both `None`-safe.
    fn bearer_token(client: &reqwest::blocking::Client) -> Option<String> {
        if let Some(bearer) = fetch_creds_token(client) {
            return Some(bearer);
        }
        fetch_anon_token(client)
    }
    let token = bearer_token(client)?;
    // reqwest 0.13's blocking builder has no `.query()` — the params ride
    // in the URL via the house urlencode (the classic escape rules the
    // token/search endpoints expect).
    let url = format!(
        "{SEARCH_URL}?type=track&limit=1&q={}",
        crate::util::urlencode(query)
    );
    let resp = client
        .get(url)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .timeout(Duration::from_secs(6))
        .send()
        .ok()?;
    if !resp.status().is_success() {
        liblog(format!(
            "spotify: search -> HTTP {}",
            resp.status().as_u16()
        ));
        return None;
    }
    parse_search(&resp.text().ok()?)
}

/// The parser half, offline-testable: `tracks.items[0]` of the search
/// response, untyped JSON-path reads (the yt layer's house style).
pub(crate) fn parse_search(json: &str) -> Option<SpotifyTrack> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let item = v.get("tracks")?.get("items")?.get(0)?.as_object()?;
    if item.is_empty() {
        return None;
    }
    let title = item.get("name").and_then(|v| v.as_str())?.to_string();
    let artists: Vec<String> = item
        .get("artists")?
        .as_array()?
        .iter()
        .filter_map(|a| a.get("name").and_then(|n| n.as_str()).map(str::to_owned))
        .collect();
    let album = item
        .get("album")?
        .get("name")
        .and_then(|v| v.as_str())?
        .to_string();
    let duration_ms = item.get("duration_ms").and_then(|v| v.as_u64())? as u32;
    let image_url = item
        .get("album")?
        .get("images")?
        .as_array()?
        .first()
        .and_then(|i| i.get("url"))
        .and_then(|u| u.as_str())
        .map(str::to_owned);
    let isrc = item
        .get("external_ids")?
        .get("isrc")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    Some(SpotifyTrack {
        title,
        artists,
        album,
        duration_ms,
        image_url,
        isrc,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_search_hit_extracts_canonical_fields() {
        let json = r#"{"tracks":{"items":[{
            "name": "Wheel in the Sky",
            "artists": [{"name": "Journey"}, {"name": "Steve Perry"}],
            "album": {"name": "Escape", "images": [{"url": "https://i.scdn.co/art"}],
                       "album_type": "album"},
            "duration_ms": 218133,
            "external_ids": {"isrc": "USSM10000242"}
        }]}}"#;
        let t = parse_search(json).expect("a hit parses");
        assert_eq!(t.title, "Wheel in the Sky");
        assert_eq!(t.artists, vec!["Journey", "Steve Perry"]);
        assert_eq!(t.album, "Escape");
        assert_eq!(t.duration_ms, 218_133);
        assert_eq!(t.image_url.as_deref(), Some("https://i.scdn.co/art"));
        assert_eq!(t.isrc.as_deref(), Some("USSM10000242"));
    }

    /// No hits, an empty items array, or a malformed body are all the same
    /// `None` — consumers degrade to YouTube-derived metadata, no noise.
    #[test]
    fn a_miss_and_garbage_are_both_none() {
        assert!(parse_search(r#"{"tracks":{"items":[]}}"#).is_none());
        assert!(parse_search("not json at all").is_none());
        assert!(parse_search(r#"{"tracks":{"items":[{"name":null}]}}"#).is_none());
    }

    #[test]
    fn an_empty_query_is_an_instant_none() {
        // No token fetch, no request — the call is free for blank input.
        assert!(search_track(&reqwest::blocking::Client::new(), "  ").is_none());
    }
}

/// Live gate: the keyless route is the whole premise — the anonymous
/// token endpoint answers and a real search parses. `#[ignore]`d for
/// CI (needs network); run with `--ignored spotify`.
#[test]
#[ignore]
fn the_keyless_route_resolves_a_real_track() {
    let client = reqwest::blocking::Client::new();
    let t = search_track(&client, "Journey Wheel in the Sky").expect("live hit");
    assert!(!t.artists.is_empty(), "a real hit has artists: {t:?}");
    assert!(t.duration_ms > 0, "a real hit has a duration");
    // The album art choice: the long-edge url from the hit.
    assert!(t.image_url.is_some(), "hits carry an image url");
}
