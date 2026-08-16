//! The resolver seam between the app and the engine.
//!
//! Everything the engine plays arrives as an opaque URI (`yt:video:…`); the
//! [`Expander`] turns those into direct stream URLs plus the metadata a local
//! player needs. Kept behind a trait so the engine never learns a provider's
//! scheme:
//!
//! - Phase 2 (app side, `src/hybrid_expander.rs`, deleted in phase 3): maps
//!   `spotify:` tracks through the Web API + YouTube search, so the mid-port
//!   app stays playable while the api layer still produces Spotify URIs.
//! - [`YtExpander`]: pure YouTube — the permanent end state, and what
//!   `examples/probe` drives.
//!
//! One-way dependency, like `api/`: expansion and resolution spawn the yt-dlp
//! CLI and hand plain data back; nothing here touches `App` or the render tree.
//! All parse/format logic is offline-testable on canned `-J` JSON (phase 1's
//! conventions); live behavior is exercised by `examples/probe`.

use crate::config;
use crate::util::{track_id_from_uri, uri_parts};
use crate::yt;

/// One resolved, playable track: the direct stream URL plus the metadata that
/// playback events and the NowPlaying pipeline consume.
#[derive(Clone)]
pub struct ResolvedTrack {
    /// The canonical uri events and metadata carry (`spotify:track:…` /
    /// `yt:video:…`). The engine echoes it untouched — it is the app's key.
    pub uri: String,
    /// A direct audio URL, valid for the session (re-resolve on recovery).
    pub url: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_ms: Option<u32>,
    pub thumbnail: Option<String>,
}

/// How many radio tracks (besides the seed) a station expands to. Mixes run
/// hundreds deep; the app's queue and memory want a sane slice.
pub const RADIO_LIMIT: usize = 50;

/// Turns a user-facing URI into a flat play queue of playable track uris,
/// resolves each to a stream, and seeds radio stations.
pub trait Expander: Send + Sync {
    /// Expand `uri` into the flat list of track uris it represents, in play
    /// order. A bare track passes through as itself; playlists/albums/artists
    /// expand into their tracks. Errors are user-facing ("couldn't play…").
    fn expand(&self, uri: &str) -> Result<Vec<String>, String>;

    /// Resolve ONE track uri to a direct stream URL (+ the metadata it comes
    /// with). Called from the engine's worker thread for every track start.
    fn resolve(&self, uri: &str) -> Result<ResolvedTrack, String>;

    /// The radio station for a seed track: the seed followed by similar uris.
    fn radio(&self, seed: &str) -> Result<Vec<String>, String>;
}

/// The pure-YouTube expander — the port's end state, live from phase 1's
/// `yt/` module.
#[derive(Default)]
pub struct YtExpander;

impl YtExpander {
    fn playlist_url(id: &str) -> String {
        format!("https://www.youtube.com/playlist?list={id}")
    }
    fn channel_videos_url(id: &str) -> String {
        format!("https://www.youtube.com/channel/{id}/videos")
    }
}

impl Expander for YtExpander {
    fn expand(&self, uri: &str) -> Result<Vec<String>, String> {
        let Some(("yt", kind, id)) = uri_parts(uri) else {
            return Err(format!("not a YouTube uri: {uri}"));
        };
        let uris = match kind {
            "video" => vec![uri.to_string()],
            "playlist" => entries_of(&Self::playlist_url(id))?,
            // YouTube has no first-class albums; a search-backed expansion is
            // the honest approximation (artist/album pages are a later phase).
            "album" => yt::search(id, config::get().search_limit)
                .into_iter()
                .map(|v| v.uri)
                .collect(),
            "channel" => entries_of(&Self::channel_videos_url(id))?,
            _ => return Err(format!("unsupported yt kind: {kind}")),
        };
        if uris.is_empty() {
            return Err(format!("{uri} expanded to nothing"));
        }
        Ok(uris)
    }

    fn resolve(&self, uri: &str) -> Result<ResolvedTrack, String> {
        let Some(id) = track_id_from_uri(uri) else {
            return Err(format!("not a track uri: {uri}"));
        };
        yt::resolve(&id)
            .map(ResolvedTrack::from)
            .ok_or_else(|| format!("couldn't resolve {uri}"))
    }

    fn radio(&self, seed: &str) -> Result<Vec<String>, String> {
        let Some(id) = track_id_from_uri(seed) else {
            return Err(format!("not a track uri: {seed}"));
        };
        // `radio_entries` caps the mix fetch to one inner-page (a full RD mix
        // paginates 15+ API calls and blows past the app's radio deadline),
        // falls back across the mix id variants, and ends in a search-built
        // pseudo-radio when YouTube has no mix for the seed at all.
        let rows = yt::radio_entries(&id);
        station_from(seed, rows)
    }
}

/// The seed first, then the mix rows — the seed itself skipped when the mix
/// echoes it, the whole list capped to `RADIO_LIMIT` + 1. Pure (no network), so
/// the station-shape logic is unit-tested offline; only the mix fetch is live.
fn station_from(seed: &str, rows: Vec<yt::YtVideo>) -> Result<Vec<String>, String> {
    let mut uris = vec![seed.to_string()];
    for row in rows.into_iter().take(RADIO_LIMIT) {
        if row.uri != seed {
            uris.push(row.uri);
        }
    }
    if uris.len() == 1 {
        return Err(format!("radio station for {seed} came back empty"));
    }
    Ok(uris)
}

fn entries_of(url: &str) -> Result<Vec<String>, String> {
    let rows = yt::playlist_entries(url);
    if rows.is_empty() {
        return Err(format!("{url} yielded no entries"));
    }
    Ok(rows.into_iter().map(|r| r.uri).collect())
}

impl From<yt::StreamInfo> for ResolvedTrack {
    fn from(s: yt::StreamInfo) -> Self {
        ResolvedTrack {
            uri: s.video.uri,
            url: s.url,
            title: s.video.title,
            artist: s.video.artist,
            album: s.video.album,
            duration_ms: s.video.duration_ms,
            thumbnail: s.video.thumbnail,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radio_prepends_the_seed_and_caps_the_slice() {
        let seed = "yt:video:dQw4w9WgXcQ".to_string();
        let row = |id: &str| yt::YtVideo {
            id: id.to_string(),
            uri: format!("yt:video:{id}"),
            title: String::new(),
            artist: String::new(),
            album: None,
            duration_ms: None,
            thumbnail: None,
        };
        // The seed first; mix rows after it; a mix that echoes the seed skips
        // the echo rather than playing it twice.
        let rows = vec![
            row("dQw4w9WgXcQ"), // the mix restarts with the seed itself
            row("aaaaaaaaaaa"),
            row("bbbbbbbbbbb"),
        ];
        let uris = station_from(&seed, rows).unwrap();
        assert_eq!(
            uris,
            vec![
                seed,
                "yt:video:aaaaaaaaaaa".into(),
                "yt:video:bbbbbbbbbbb".into()
            ]
        );
    }

    #[test]
    fn radio_caps_at_radio_limit_and_rejects_empty_stations() {
        let seed = "yt:video:dQw4w9WgXcQ".to_string();
        let row = |i: u32| yt::YtVideo {
            id: format!("a{i:010}"),
            uri: format!("yt:video:a{i:010}"),
            title: String::new(),
            artist: String::new(),
            album: None,
            duration_ms: None,
            thumbnail: None,
        };
        // 2× RADIO_LIMIT rows in, the slice still stops at RADIO_LIMIT + seed.
        let rows: Vec<yt::YtVideo> = (0..RADIO_LIMIT as u32 * 2).map(row).collect();
        let uris = station_from(&seed, rows).unwrap();
        assert_eq!(uris.len(), RADIO_LIMIT + 1);
        assert_eq!(uris[0], seed);
        // Nothing but the seed itself → an empty station is an error, not a
        // one-track station.
        let rows = vec![yt::YtVideo {
            id: "dQw4w9WgXcQ".to_string(),
            uri: seed.clone(),
            title: String::new(),
            artist: String::new(),
            album: None,
            duration_ms: None,
            thumbnail: None,
        }];
        assert!(station_from(&seed, rows).is_err());
    }

    #[test]
    fn video_uris_expand_to_themselves() {
        let uris = YtExpander.expand("yt:video:dQw4w9WgXcQ").unwrap();
        assert_eq!(uris, vec!["yt:video:dQw4w9WgXcQ".to_string()]);
    }

    #[test]
    fn unknown_schemes_are_rejected_with_a_reason() {
        assert!(YtExpander.expand("spotify:playlist:xyz").is_err());
        assert!(YtExpander.expand("yt:video").is_err());
        assert!(YtExpander.expand("yt:podcast:x").is_err());
    }

    #[test]
    fn resolve_rejects_non_track_uris() {
        assert!(YtExpander.resolve("yt:playlist:PLabc").is_err());
        assert!(YtExpander.resolve("").is_err());
    }

    /// Live smoke test: needs yt-dlp + network. Run with `--ignored`.
    #[test]
    #[ignore]
    fn live_radio_roundtrip() {
        let uris = YtExpander
            .radio("yt:video:dQw4w9WgXcQ")
            .expect("radio station");
        assert!(uris.len() >= 2, "seed + at least one similar track");
        assert_eq!(uris[0], "yt:video:dQw4w9WgXcQ");
        assert!(uris.iter().all(|u| u.starts_with("yt:video:")));
    }

    /// Live fallback probe: a seed whose mix is empty (fresh upload, or its
    /// player endpoint bot-gated) must still yield a station via the
    /// search-built pseudo-radio. The seed here was chosen as one YouTube
    /// serves no `RD` mix for; if it ever gains a mix the test still passes —
    /// any non-empty station whose first row is the seed is the contract.
    #[test]
    #[ignore]
    fn live_radio_falls_back_to_a_search_station() {
        let uris = YtExpander
            .radio("yt:video:P8qNOneERe0")
            .expect("radio station");
        assert_eq!(uris[0], "yt:video:P8qNOneERe0");
        assert!(uris.len() >= 2, "seed + at least one similar track");
    }
}
