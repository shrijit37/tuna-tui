//! Small pure helpers shared by the UI and the workers.
//!
//! Everything here is dependency-light and side-effect free, so it can be
//! unit-tested without a terminal, a network, or an audio device.

use ratatui::layout::Rect;
use std::time::Duration;

/// Truncate to `max` characters, replacing the tail with an ellipsis.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    } else {
        s.to_string()
    }
}

/// Format milliseconds as `m:ss`.
pub fn fmt_ms(ms: u32) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Convert a 0..=100 percentage to the engine's 0..=65535 volume range.
pub fn vol_u16(pct: u8) -> u16 {
    (pct as u32 * 65535 / 100) as u16
}

/// One step of an exponential retry: double the current wait, capped.
///
/// Shared by the engine's recovery loop and the txc subscriber's reconnect
/// loop — each keeps its own start/cap policy, only the shape is common.
pub fn backoff_step(current: Duration, cap: Duration) -> Duration {
    (current * 2).min(cap)
}

/// Vertically center a `height`-row rect inside `area`.
pub fn center_v(area: Rect, height: u16) -> Rect {
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect {
        x: area.x,
        y,
        width: area.width,
        height: height.min(area.height),
    }
}

/// Split any `scheme:kind:id` URI into its three parts. The port's `yt:`
/// URIs parse here, as do the synthetic `tuna:action:` rows; consumers that
/// care about kind match on it by name, not position.
pub fn uri_parts(uri: &str) -> Option<(&str, &str, &str)> {
    let mut p = uri.split(':');
    match (p.next(), p.next(), p.next()) {
        // Lenient by design: matches the pre-port contract (tests/util.rs locks
        // in trailing-segment tolerance and empty-id acceptance as quirks).
        (Some(scheme), Some(kind), Some(id)) => Some((scheme, kind, id)),
        _ => None,
    }
}

/// Convert a `yt:kind:id` URI to its youtube.com equivalent. Other schemes
/// (the synthetic `tuna:action:` rows) have no shareable URL and return "".
/// The video/playlist shapes delegate to the single-owner builders below;
/// the channel card (`/channel/{id}`) stays distinct from the uploads tab
/// ([`channel_videos_url`]) that drill-ins expand.
pub fn uri_to_url(uri: &str) -> String {
    let Some((scheme, kind, id)) = uri_parts(uri) else {
        return String::new();
    };
    match (scheme, kind) {
        ("yt", "video") => video_url(uri).unwrap_or_default(),
        ("yt", "playlist") => playlist_uri(id),
        ("yt", "channel") => format!("https://www.youtube.com/channel/{id}"),
        _ => String::new(),
    }
}

/// Normalize a video id / `yt:video:` uri / watch URL to a watch URL. These
/// builders own the canonical youtube.com shapes, so every layer (the yt
/// resolver, the expander, browse) shares one spelling.
pub fn video_url(url_or_id: &str) -> Option<String> {
    if url_or_id.starts_with("http://") || url_or_id.starts_with("https://") {
        return Some(url_or_id.to_string());
    }
    if let Some(id) = track_id_from_uri(url_or_id) {
        return Some(format!("https://www.youtube.com/watch?v={id}"));
    }
    // A bare id — but only a bare one: a `yt:playlist:` or other non-video uri
    // must not masquerade as a video id.
    if url_or_id.contains(':') {
        return None;
    }
    (!url_or_id.is_empty()).then(|| format!("https://www.youtube.com/watch?v={url_or_id}"))
}

/// The canonical playlist URL.
pub fn playlist_uri(id: &str) -> String {
    format!("https://www.youtube.com/playlist?list={id}")
}

/// The canonical channel uploads tab — what a `yt:channel:` drill-in expands.
pub fn channel_videos_url(id: &str) -> String {
    format!("https://www.youtube.com/channel/{id}/videos")
}

/// Pull the id out of a `yt:video:<id>` URI.
pub fn track_id_from_uri(uri: &str) -> Option<String> {
    match uri_parts(uri) {
        Some(("yt", "video", id)) => Some(id.to_string()),
        _ => None,
    }
}

/// Percent-encode a string for use in a query component.
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // The lenient contract (trailing segments, empty id) is locked in by the
    // integration suite in tests/util.rs; these tests cover the yt: side the
    // port adds.

    #[test]
    fn uri_parts_reads_the_id_position_for_both_schemes() {
        assert_eq!(
            uri_parts("yt:video:dQw4w9WgXcQ"),
            Some(("yt", "video", "dQw4w9WgXcQ"))
        );
        assert_eq!(
            uri_parts("yt:playlist:PLabc"),
            Some(("yt", "playlist", "PLabc"))
        );
        assert_eq!(
            uri_parts("tuna:action:liked-play"),
            Some(("tuna", "action", "liked-play"))
        );
        // The action scheme is read scheme-agnostically: rows written before
        // the tuna-tui rename carried `myx:` and still parse (and yield no URL).
        assert_eq!(
            uri_parts("myx:action:liked-play"),
            Some(("myx", "action", "liked-play"))
        );
    }

    #[test]
    fn uri_to_url_youtube_mappings() {
        assert_eq!(
            uri_to_url("yt:video:dQw4w9WgXcQ"),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        );
        assert_eq!(
            uri_to_url("yt:playlist:PLabc"),
            "https://www.youtube.com/playlist?list=PLabc"
        );
        assert_eq!(
            uri_to_url("yt:channel:UCabc"),
            "https://www.youtube.com/channel/UCabc"
        );
    }

    #[test]
    fn track_id_from_uri_yt_video_kind() {
        assert_eq!(
            track_id_from_uri("yt:video:dQw4w9WgXcQ"),
            Some("dQw4w9WgXcQ".into())
        );
        assert_eq!(track_id_from_uri("yt:playlist:PLabc"), None); // not a video
    }
}
