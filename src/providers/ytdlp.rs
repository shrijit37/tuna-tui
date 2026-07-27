//! yt-dlp adapter — the one real backend today. Wraps `yt::search` /
//! `yt::resolve` and normalizes into the canonical `contracts::Song` /
//! `PlaybackInfo` shapes. When the InnerTube client lands (Myx-mh7.1/.2) it
//! becomes a sibling producer of these same types; no other layer changes.

use super::contracts::{AlbumRef, ArtistRef, AudioStream, PlaybackInfo, Song, Thumbnail};

/// Flat yt-dlp search, normalized to canonical `Song`s.
pub fn search(query: &str, limit: usize) -> Result<Vec<Song>, String> {
    let vids = crate::yt::search(query, limit);
    Ok(vids.into_iter().map(yt_video_to_song).collect())
}

/// Resolve a video id into playback info (stream URL + metadata).
pub fn resolve_stream(id: &str) -> Result<PlaybackInfo, String> {
    let info = crate::yt::resolve(id).ok_or_else(|| format!("yt-dlp resolve failed for {id}"))?;
    // Map StreamInfo (existing) -> PlaybackInfo.
    Ok(PlaybackInfo {
        id: id.to_string(),
        expires_at: None,
        audio: vec![AudioStream {
            url: info.url.clone(),
            mime_type: "audio/mp4".to_string(),
            codec: None,
            bitrate: None,
            sample_rate: Some(48000),
            channels: Some(2),
            content_length: None,
            itag: None,
        }],
        title: info.video.title,
        artist: info.video.artist,
        album: info.video.album,
        duration_ms: info.video.duration_ms,
        thumbnail: info.video.thumbnail,
    })
}

/// The mapper is the normalization boundary in miniature: whatever yt-dlp's
/// `-J` dump says, the core only ever sees a `Song`.
fn yt_video_to_song(v: crate::yt::YtVideo) -> Song {
    let id = crate::util::track_id_from_uri(&v.uri).unwrap_or_else(|| v.uri.clone());
    let thumb = v
        .thumbnail
        .map(|url| Thumbnail {
            url,
            width: 0,
            height: 0,
        })
        .into_iter()
        .collect();
    Song {
        id,
        title: v.title,
        subtitle: None,
        artists: if v.artist.is_empty() {
            vec![]
        } else {
            vec![ArtistRef {
                id: None,
                name: v.artist,
            }]
        },
        album: v.album.map(|name| AlbumRef { id: None, name }),
        duration_ms: v.duration_ms,
        thumbnails: thumb,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yt_video_to_song_maps_thumbnail() {
        let v = crate::yt::YtVideo {
            uri: "yt:video:dQw4w9WgXcQ".into(),
            title: "Test".into(),
            artist: "Artist".into(),
            album: None,
            duration_ms: Some(213000),
            thumbnail: Some("https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg".into()),
        };
        let s = yt_video_to_song(v);
        assert_eq!(s.id, "dQw4w9WgXcQ");
        assert_eq!(s.thumbnails.len(), 1);
        assert_eq!(s.duration_ms, Some(213000));
    }
}
