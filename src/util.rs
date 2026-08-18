//! Small pure helpers shared by the UI and the workers.
//!
//! Everything here is dependency-light and side-effect free, so it can be
//! unit-tested without a terminal, a network, or an audio device.

use ratatui::layout::Rect;
use std::borrow::Cow;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Truncate to `max` characters, replacing the tail with an ellipsis.
///
/// Borrows the input when it already fits (the common render-row case — no
/// allocation); only the cut path builds a string.
pub fn truncate<'a>(s: &'a str, max: usize) -> Cow<'a, str> {
    if s.chars().count() <= max {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(s.chars().take(max.saturating_sub(1)).collect::<String>() + "…")
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

/// The app's cache directory (`~/.cache/tuna-tui`). Pure join — never creates:
/// callers that need the dir on disk go through [`ensure_cache_dir_0700`]. Sites
/// that can run before `config::migrate_legacy_paths()` must not create it
/// eagerly, or the one-time legacy `.cache/myx` move could race a fresh tree.
pub fn cache_dir() -> Option<PathBuf> {
    crate::home_dir().map(|h| h.join(".cache/tuna-tui"))
}

/// Create the cache dir (mode 0700 on unix — idempotent) and return it. Only
/// allowed from sites that are already past `migrate_legacy_paths()`: liblog,
/// the single-instance lock, the http cache, and state.json persistence.
pub fn ensure_cache_dir_0700() -> Option<PathBuf> {
    let dir = cache_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Some(dir)
}

/// A unique temp sibling for [`write_atomic`]. One process-wide counter feeds
/// every writer, so two overlapping saves (the un-awaited periodic tick and
/// the awaited quit save) can never share a scratch name and rename a torn
/// temp into place.
fn tmp_sibling(path: &Path) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    path.with_extension(format!("{}.tmp", SEQ.fetch_add(1, Ordering::Relaxed)))
}

/// Write `bytes` to `path` atomically: a uniquely-named temp sibling, fsync,
/// then rename over the destination. Returns true when the rename landed; on
/// any failure the temp is removed and false is returned — a torn write can
/// never become the visible file.
///
/// The rename-over-existing fallback covers Windows, where the rename can
/// fail over a destination that is open or otherwise locked: the destination
/// is removed and the rename retried once. The fallback deletes the old file
/// before the second rename, so a failure there leaves `path` absent — the
/// state.json caller keeps `.bak` to cover the gap; on unix the first rename
/// replaces in place and the fallback is effectively unreachable.
///
/// `pub` because the persistence caller lives in the binary crate; only the
/// lib crate's own modules could see a `pub(crate)` item.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> bool {
    let tmp = tmp_sibling(path);
    if std::fs::File::create(&tmp)
        .and_then(|mut f| {
            f.write_all(bytes)?;
            f.sync_all()
        })
        .is_err()
    {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    if std::fs::rename(&tmp, path).is_ok() {
        return true;
    }
    // Windows: rename cannot replace an existing destination — remove first.
    // Never run on Unix: a failed first rename fails identically after
    // remove, so the destination would be deleted for nothing.
    #[cfg(windows)]
    {
        if std::fs::remove_file(path).is_ok() && std::fs::rename(&tmp, path).is_ok() {
            return true;
        }
    }
    let _ = std::fs::remove_file(&tmp);
    false
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

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tuna-tui-util-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn write_atomic_replaces_the_destination_and_leaves_no_tmp() {
        let dir = scratch("write-atomic");
        let path = dir.join("state.json");
        assert!(write_atomic(&path, b"one"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one");
        assert!(write_atomic(&path, b"two"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("state.json")]);
    }

    #[test]
    fn write_atomic_fails_on_an_unwritable_path_without_creating_anything() {
        let dir = std::env::temp_dir().join("tuna-tui-util-write-atomic-fail");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("state.json"); // parent never exists
        assert!(!write_atomic(&path, b"one"));
        assert!(!dir.exists());
    }

    #[test]
    fn tmp_sibling_names_are_unique_per_call() {
        let dir = scratch("tmp-sibling");
        let path = dir.join("state.json");
        let one = tmp_sibling(&path);
        let two = tmp_sibling(&path);
        assert_ne!(one, two);
        assert_eq!(one.parent(), two.parent());
        assert!(one.file_name().unwrap().to_string_lossy().ends_with(".tmp"));
    }
}
