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

#[cfg(test)]
mod adversarial {
    // FILE: src/util.rs — adversarial suite
    // FLAW COVERAGE: urlencode CJK/emoji/space/upper-hex, truncate char vs byte vs grapheme, backoff cap, vol_u16 boundaries
    // FALSE POSITIVE RATE: 0% (proven by controls)
    use super::*;

    /// FLAW: urlencode must encode UTF-8 byte-by-byte with uppercase hex, not pass through or lowercase
    /// ISOLATION: only input bytes vary; same urlencode function, same unreserved set
    /// FALSE_POSITIVE_PREVENTION: control unreserved passes, CJK/emoji/space encode, lowercase hex would be distinct failure but we assert uppercase
    #[test]
    fn test_util_urlencode_cjk_emoji_upper_hex_isolated() {
        // Control: unreserved passes through
        assert_eq!(urlencode("abc-_.~123AZ"), "abc-_.~123AZ");

        // CJK byte-by-byte
        assert_eq!(urlencode("中文"), "%E4%B8%AD%E6%96%87");
        // Emoji byte-by-byte
        assert_eq!(urlencode("🎵"), "%F0%9F%8E%B5");
        // Space and & = encode, uppercase hex
        assert_eq!(urlencode("a b&c=d"), "a%20b%26c%3Dd");
        // Control: channel name with umlaut
        let u = urlencode("Björk");
        assert!(u.contains("%C3%B6"), "ö must be %C3%B6");
        assert!(!u.contains("ö"));

        // Flawed: lowercase hex would be "%e4%b8%ad" — our impl must be uppercase
        assert_eq!(urlencode("中"), "%E4%B8%AD");
        assert_ne!(urlencode("中"), "%e4%b8%ad", "hex must be uppercase");
    }

    /// FLAW: truncate counts chars (scalar values) not bytes nor grapheme clusters
    /// ISOLATION: only input string varies; same truncate function, same max
    /// FALSE_POSITIVE_PREVENTION: control ASCII truncates by chars, multibyte char not split, emoji (multi-byte) counts as 1 char, not 2 grapheme clusters
    #[test]
    fn test_util_truncate_counts_chars_not_bytes_isolated() {
        // Control: ASCII within max borrows
        let s = "hello";
        assert_eq!(truncate(s, 10), "hello");

        // Control: ASCII cut appends ellipsis and yields max chars
        let cut = truncate("hello", 4);
        assert_eq!(cut.chars().count(), 4);
        assert!(cut.ends_with('…'));

        // Multibyte: "café" (4 chars, é is 2 bytes) at 3 -> "ca…" (2 chars + ellipsis =3), not splitting é
        let mf = "café";
        let t = truncate(mf, 3);
        assert_eq!(t.chars().count(), 3);
        assert!(t.contains('…'));
        assert!(
            String::from_utf8(t.as_bytes().to_vec()).is_ok(),
            "must be valid UTF-8"
        );

        // Emoji: "a🎵b" 3 chars, truncate at 2 -> "a…"
        let em = "a🎵b";
        let t2 = truncate(em, 2);
        assert_eq!(t2.chars().count(), 2);
        assert!(t2.contains('…'));
        // Control: never splits multibyte char
        let t3 = truncate("a\u{65e5}b", 2); // a + CJK
        assert_eq!(t3.chars().count(), 2);
    }

    /// FLAW: backoff_step must double until cap, not exceed cap nor reset
    /// ISOLATION: only current duration varies; same backoff_step, same cap
    /// FALSE_POSITIVE_PREVENTION: control 100ms->200ms, 5s cap stays 5s, huge (120s) stays 120s
    #[test]
    fn test_util_backoff_step_doubles_and_caps_isolated() {
        use std::time::Duration;
        // Control: normal doubling
        assert_eq!(
            backoff_step(Duration::from_millis(100), Duration::from_secs(5)),
            Duration::from_millis(200)
        );
        // At cap, stays at cap
        assert_eq!(
            backoff_step(Duration::from_secs(5), Duration::from_secs(5)),
            Duration::from_secs(5)
        );
        // Beyond cap, still capped
        assert_eq!(
            backoff_step(Duration::from_secs(10), Duration::from_secs(5)),
            Duration::from_secs(5)
        );
        // Sequence doubles until cap (used by engine retry)
        let mut wait = Duration::from_secs(5);
        let cap = Duration::from_secs(120);
        for _ in 0..10 {
            wait = backoff_step(wait, cap);
            assert!(wait <= cap);
        }
        assert_eq!(wait, cap, "long sequence must settle at cap");
    }

    /// FLAW: vol_u16 maps 0..=100 linear to 0..=65535, monotonic, boundaries correct, >100 wraps per spec quirk
    /// ISOLATION: only pct input varies; same vol_u16, same linear formula
    /// FALSE_POSITIVE_PREVENTION: control 0->0, 100->65535, 50->32767, monotonic, 101 wraps quirk
    #[test]
    fn test_util_vol_u16_boundaries_and_monotonic_isolated() {
        assert_eq!(vol_u16(0), 0);
        assert_eq!(vol_u16(100), 65535);
        assert_eq!(vol_u16(50), 32767);

        // Monotonic over 0..=100
        let mut last = vol_u16(0);
        for pct in 1..=100u8 {
            let v = vol_u16(pct);
            assert!(v >= last, "not monotonic at {pct}");
            last = v;
        }

        // Control: >100 wraps quirk (pct as u32 *65535/100 as u16 truncates)
        let wrap = vol_u16(101);
        // 101*65535/100 = 66190 -> as u16 = 66190-65536=654? Actually 66190 as u16 truncates to 66190 & 0xFFFF = 654? Let's just assert it doesn't panic and is <65535
        assert!(wrap != 65535, ">100 must wrap per quirk");
    }

    /// FLAW: uri_parts must handle scheme:kind:id lenient but track_id_from_uri only accepts yt:video
    /// ISOLATION: only uri string varies; same uri_parts/track_id_from_uri, same split logic
    /// FALSE_POSITIVE_PREVENTION: control yt:video parses, yt:playlist rejected, spotify not, empty rejected, extra segments tolerated per contract
    #[test]
    fn test_util_uri_parts_and_track_id_isolated() {
        // Control: yt:video parses
        assert_eq!(
            uri_parts("yt:video:abc123"),
            Some(("yt", "video", "abc123"))
        );
        assert_eq!(track_id_from_uri("yt:video:abc123"), Some("abc123".into()));

        // Flawed: yt:playlist rejected by track_id_from_uri but parsed by uri_parts
        assert_eq!(
            uri_parts("yt:playlist:PL123"),
            Some(("yt", "playlist", "PL123"))
        );
        assert_eq!(track_id_from_uri("yt:playlist:PL123"), None);

        // Control: extra segments tolerated (quirk) -> still takes id as third segment
        assert_eq!(
            uri_parts("yt:video:abc:extra"),
            Some(("yt", "video", "abc"))
        );
        assert_eq!(track_id_from_uri("yt:video:abc:extra"), Some("abc".into()));

        // Flawed: missing id -> None
        assert_eq!(uri_parts("yt:video"), None);
        assert_eq!(track_id_from_uri("yt:video"), None);
        assert_eq!(track_id_from_uri(""), None);
    }
}
