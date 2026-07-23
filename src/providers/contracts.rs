//! Canonical DTOs — the normalization boundary between upstream backends
//! (yt-dlp today, InnerTube later) and the tuna-tui core. Every song enters
//! the core as one of these shapes; upstream-specific JSON never leaks past
//! the adapter in `ytdlp.rs`.
//!
//! Deliberately plain data: no traits, no router, no error taxonomy — those
//! return the day a second real backend needs to be interchangeable with
//! yt-dlp (beads Myx-mh7.1/.2/.4/.5).

use std::time::SystemTime;

/// Normalized song.
#[derive(Debug, Clone)]
pub struct Song {
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub artists: Vec<ArtistRef>,
    pub album: Option<AlbumRef>,
    pub duration_ms: Option<u32>,
    pub thumbnails: Vec<Thumbnail>,
}

#[derive(Debug, Clone)]
pub struct ArtistRef {
    pub id: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct AlbumRef {
    pub id: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Thumbnail {
    pub url: String,
    pub width: u32,
    pub height: u32,
}

/// Playback info. Audio/video split kept from the spec (§33); `expires_at`
/// is the documented hook for cached-URL resume (§34).
#[derive(Debug, Clone)]
pub struct PlaybackInfo {
    pub id: String,
    pub expires_at: Option<SystemTime>,
    pub audio: Vec<AudioStream>,
    // Carried from yt::StreamInfo::video for ResolvedTrack conversion.
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_ms: Option<u32>,
    pub thumbnail: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AudioStream {
    pub url: String,
    pub mime_type: String,
    pub codec: Option<String>,
    pub bitrate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
    pub content_length: Option<u64>,
    pub itag: Option<String>,
}
