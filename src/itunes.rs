//! Keyless canonical-music metadata — the cred-free answer to the quality
//! goal (no Spotify credentials, no anonymous-token gray zone).
//!
//! Leg: Apple's iTunes Search API (`itunes.apple.com/search`), keyless and
//! official. `media=music&entity=song` returns ONLY songs — podcasts,
//! audiobooks, and non-music content are structurally absent from the
//! result set, which is exactly the "music-only TUI" property: rows built
//! from these hits are songs by construction, not by heuristic.
//!
//! Every consumer treats a miss as "keep the YouTube-derived metadata":
//! the fetch functions return `None`/empty on any failure and the engine
//! falls back silently — the feature degrades, it never breaks playback.

use std::time::Duration;

use crate::liblog::liblog;

/// One canonical song hit from the iTunes search API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SongHit {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u32,
    /// Scaled artwork at the 600px variant (Apple's URL size-swap).
    pub artwork_url: Option<String>,
    pub isrc: Option<String>,
}

const SEARCH_URL: &str = "https://itunes.apple.com/search";
const TIMEOUT: Duration = Duration::from_secs(6);

/// Search for up to `limit` canonical songs. Empty on any failure.
pub fn search_songs(client: &reqwest::blocking::Client, query: &str, limit: usize) -> Vec<SongHit> {
    if query.trim().is_empty() || limit == 0 {
        return Vec::new();
    }
    // reqwest 0.13's blocking builder has no `.query()` — params ride in
    // the URL via the house urlencode.
    let url = format!(
        "{SEARCH_URL}?media=music&entity=song&limit={limit}&term={}",
        crate::util::urlencode(query)
    );
    let Ok(resp) = client.get(url).timeout(TIMEOUT).send() else {
        return Vec::new();
    };
    if !resp.status().is_success() {
        liblog(format!("itunes: search -> HTTP {}", resp.status().as_u16()));
        return Vec::new();
    }
    let Ok(body) = resp.text() else {
        return Vec::new();
    };
    parse_songs(&body)
}

/// The top hit, for the engine's per-track enrichment.
pub fn search_track(client: &reqwest::blocking::Client, query: &str) -> Option<SongHit> {
    search_songs(client, query, 1).into_iter().next()
}

/// Confidence gate before a hit may override the YouTube-derived row: the
/// search is fuzzy (free-form `artist title` term), so a top hit can be a
/// DIFFERENT song. A wrong hit must not be applied as truth — only a hit
/// whose artist shares a token with the source row (the artist field or the
/// title) may. "Journey Wheel in the Sky" -> hit artist "Journey" overlaps;
/// a hits-are-random miss yields no overlap and the row keeps its own
/// metadata. Failure and wrongness are distinct: `search_track`'s `None`
/// covers failure; this covers wrongness.
pub(crate) fn tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

pub(crate) fn artist_overlap(yts_artist: &str, yts_title: &str, hit_artist: &str) -> bool {
    let mut source: Vec<String> = tokens(yts_artist);
    source.extend(tokens(yts_title));
    let hit = tokens(hit_artist);
    source.iter().any(|a| hit.contains(a))
}

/// Confidence gate for the search MAPPING seam (#22): the canonical hit is
/// played through the top video of an `artist title single` search, and a
/// single title-token match still lets covers through ("Wheel in the Sky
/// but it's a lofi cover" shares "sky"). Requiring the canonical ARTIST to
/// appear in the video's title or artist field closes that: a cover's
/// title rarely names the original artist. Miss = drop the row.
pub fn video_matches(
    hit_artist: &str,
    hit_title: &str,
    video_title: &str,
    video_artist: &str,
) -> bool {
    let artist = tokens(hit_artist);
    let title = tokens(hit_title);
    let vtitle = tokens(video_title);
    let vartist = tokens(video_artist);
    let artist_ok = artist.is_empty()
        || artist
            .iter()
            .any(|t| vtitle.contains(t) || vartist.contains(t));
    let title_ok = title.is_empty() || title.iter().any(|t| vtitle.contains(t));
    artist_ok && title_ok
}

/// The parser half, offline-testable: `results[0..]` of the iTunes search
/// response, untyped JSON-path reads (the yt layer's house style).
pub(crate) fn parse_songs(json: &str) -> Vec<SongHit> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(results) = v.get("results").and_then(|r| r.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for r in results {
        let title = match r.get("trackName").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => continue, // a non-song row (entity drift) is skipped, not coerced
        };
        let Some(artist) = r
            .get("artistName")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
        else {
            continue;
        };
        let album = r
            .get("collectionName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let duration_ms = r
            .get("trackTimeMillis")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        // The 100px artwork url upscales by size-swap (Apple's documented
        // trick): "100x100bb" -> "600x600bb" keeps the same CDN object.
        let artwork_url = r
            .get("artworkUrl100")
            .and_then(|v| v.as_str())
            .map(|u| u.replace("100x100bb", "600x600bb"));
        let isrc = r.get("isrc").and_then(|v| v.as_str()).map(str::to_owned);
        out.push(SongHit {
            title,
            artist,
            album,
            duration_ms,
            artwork_url,
            isrc,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_search_hit_extracts_canonical_fields() {
        let json = r#"{"resultCount":1,"results":[{
            "trackName": "Wheel in the Sky",
            "artistName": "Journey",
            "collectionName": "Escape",
            "trackTimeMillis": 218133,
            "artworkUrl100": "https://is1-ssl.mzstatic.com/image/thumb/100x100bb.jpg",
            "isrc": "USSM10000242",
            "primaryGenreName": "Rock"
        }]}"#;
        let hits = parse_songs(json);
        assert_eq!(hits.len(), 1);
        let h = &hits[0];
        assert_eq!(h.title, "Wheel in the Sky");
        assert_eq!(h.artist, "Journey");
        assert_eq!(h.album, "Escape");
        assert_eq!(h.duration_ms, 218_133);
        assert_eq!(
            h.artwork_url.as_deref(),
            Some("https://is1-ssl.mzstatic.com/image/thumb/600x600bb.jpg")
        );
        assert_eq!(h.isrc.as_deref(), Some("USSM10000242"));
    }

    /// Non-song rows are skipped, never coerced; garbage and misses are
    /// empty — the music-only property is structural, not heuristic.
    #[test]
    fn non_song_rows_and_garbage_are_empty() {
        let json = r#"{"results":[
            {"trackName": null, "artistName": "X"},
            {"trackName": "a", "artistName": null}
        ]}"#;
        assert!(parse_songs(json).is_empty());
        assert!(parse_songs("not json").is_empty());
        assert!(parse_songs(r#"{"results":[]}"#).is_empty());
    }

    #[test]
    fn an_empty_query_is_an_instant_empty() {
        assert!(search_songs(&reqwest::blocking::Client::new(), "  ", 5).is_empty());
    }

    /// The confidence gate: token overlap between the source row and the
    /// hit artist decides whether the hit may override. Case-insensitive,
    /// separator-agnostic; artist-in-title counts.
    #[test]
    fn the_confidence_gate_needs_artist_token_overlap() {
        assert!(artist_overlap("Journey", "Wheel in the Sky", "Journey"));
        assert!(artist_overlap("", "Wheel in the Sky Journey", "Journey"));
        assert!(artist_overlap("THE BEATLES", "Let It Be", "The Beatles"));
        assert!(artist_overlap("Tame Impala", "The Less I Know", "impala"));
        assert!(!artist_overlap("Journey", "Wheel in the Sky", "Katy Perry"));
        assert!(
            !artist_overlap("", "Wheel in the Sky", "Journey"),
            "not in the title"
        );
        assert!(!artist_overlap("", "", "Random Artist"));
    }

    /// The mapping gate (#22): the canonical row must match the playable
    /// video — official/audio/live variants keep (artist+title tokens
    /// present), covers and unrelated videos drop (a cover's title does
    /// not name the original artist).
    #[test]
    fn the_mapping_gate_keeps_matches_and_drops_covers() {
        // Official audio: "Journey - Wheel In The Sky (Official Audio)"
        assert!(video_matches(
            "Journey",
            "Wheel in the Sky",
            "Journey - Wheel In The Sky (Official Audio)",
            "Journey"
        ));
        // Live: "Wheel in the Sky (Live at...)" — artist token still there
        assert!(video_matches(
            "Journey",
            "Wheel in the Sky",
            "Wheel in the Sky (Live in Osaka)",
            "Journey"
        ));
        // A lofi cover shares title tokens but no artist token -> dropped
        assert!(!video_matches(
            "Journey",
            "Wheel in the Sky",
            "Wheel in the Sky but it's a lofi cover",
            "LoFi Beats"
        ));
        // Unrelated results drop outright
        assert!(!video_matches(
            "Journey",
            "Wheel in the Sky",
            "Top 100 Rock Songs Ever",
            "Rock Mix"
        ));
        // Case-folding + punctuation
        assert!(video_matches(
            "THE BEATLES",
            "Let It Be",
            "The Beatles - Let It Be (Remastered)",
            "The Beatles"
        ));
    }

    /// Live gate: the keyless leg is the whole premise — a real query
    /// resolves canonical songs and the artwork swap holds. `#[ignore]`d
    /// for CI (needs network); run with `-- --ignored itunes`.
    #[test]
    #[ignore]
    fn the_keyless_leg_resolves_real_songs() {
        let client = reqwest::blocking::Client::new();
        let hits = search_songs(&client, "Journey Wheel in the Sky", 3);
        assert!(!hits.is_empty(), "a live query has hits");
        let h = &hits[0];
        assert!(!h.artist.is_empty(), "a real hit has an artist: {h:?}");
        assert!(h.duration_ms > 0, "a real hit has a duration");
        assert!(h.artwork_url.is_some(), "hits carry scalable artwork");
    }
}
