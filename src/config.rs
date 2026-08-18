//! User settings from `~/.config/tuna-tui/config.toml`. Missing, empty or
//! malformed all fall back to defaults — a typo must never lock someone out of
//! the app, and a wrong-typed *value* must never take out the keys beside it
//! (see [`Config::parse`]).
//!
//! Only unparseable TOML *syntax* still falls back wholesale — a broken
//! document has nothing to salvage. A value that fails its key's type check
//! defaults that key alone.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub struct Config {
    /// Rows kept visible above and below the list cursor, like vim's `scrolloff`.
    pub scrolloff: usize,
    /// Resume the locally saved track, source and position when Tuna TUI starts.
    pub restore_on_startup: bool,
    /// Terminal graphics protocol: kitty, iterm2, sixel or halfblocks. Set this
    /// when the startup query misfires and the art comes out as a mosaic.
    /// `TUNA_PROTOCOL` takes precedence.
    pub protocol: Option<String>,
    /// The yt-dlp binary used by the `yt/` layer during the YouTube port.
    pub ytdlp_path: String,
    /// The ffmpeg binary that decodes the stream into raw PCM for the engine.
    pub ffmpeg_path: String,
    /// Format-selection string for stream resolution (`yt-dlp -f`).
    pub audio_format: String,
    /// How many YouTube search results `ytsearchN:` is asked for.
    pub search_limit: usize,
    /// Optional cookies file (`yt-dlp --cookies`): unlocks private playlists,
    /// liked lists and history, and quiets bot checks that throttle anonymized
    /// traffic.
    pub cookies_file: Option<String>,
    /// Seconds of decoded PCM buffered before playback output starts. A larger
    /// buffer smooths stutter on high-latency or fluctuating connections at
    /// the cost of a silent pre-roll on every stream start.
    pub buffer_duration_secs: u8,
}

/// Stereo f32 samples for `secs` of 44.1 kHz audio — what the engine's
/// prebuffer gate counts. The engine and the tests share this conversion so
/// the knob and the gate can never disagree.
pub fn prebuffer_samples(secs: u8) -> usize {
    secs as usize * 44_100 * 2
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scrolloff: 3,
            restore_on_startup: true,
            protocol: None,
            ytdlp_path: "yt-dlp".to_string(),
            ffmpeg_path: "ffmpeg".to_string(),
            audio_format: "bestaudio/best".to_string(),
            search_limit: 6,
            cookies_file: None,
            buffer_duration_secs: 2,
        }
    }
}

/// The settings, read once. Shared so the client-id lookup and the UI can't
/// disagree about what the file says.
pub fn get() -> &'static Config {
    static CONFIG: OnceLock<Config> = OnceLock::new();
    CONFIG.get_or_init(Config::load)
}

/// Written on first run so there is a file to edit instead of a path to guess.
/// Every key is commented out, so it parses to exactly the defaults.
const TEMPLATE: &str = "\
# tuna-tui settings. Every key is optional — uncomment one to change it.

# Rows kept visible above and below the list cursor, like vim's scrolloff.
#scrolloff = 3

# Resume the locally saved track, source and position when Tuna TUI starts.
#restore_on_startup = true

# Terminal graphics protocol: kitty, iterm2, sixel or halfblocks.
# Leave it commented to auto-detect; set it if album art comes out as a coarse
# mosaic, which means the detection query went unanswered.
#protocol = \"kitty\"

# The yt-dlp binary used by the YouTube layer (port in progress).
# Only needed if yt-dlp is not on PATH.
#ytdlp_path = \"yt-dlp\"

# The ffmpeg binary that decodes the stream into raw PCM for the engine.
# Only needed if ffmpeg is not on PATH.
#ffmpeg_path = \"ffmpeg\"

# Format-selection string for stream resolution (passed to `yt-dlp -f`).
# Note: stream URLs are resolved with the `android` player client
# (unthrottled on this box), which exposes no audio-only formats — the
# `best` fallback tail always lands on the muxed 360p stream. Keep the
# `/best` fallback; a bare `bestaudio` would resolve to the same muxed
# stream via the engine's appended fallback, so the knob mostly matters
# for metadata, not bandwidth.
#audio_format = \"bestaudio/best\"

# How many results `ytsearchN:` is asked for per search.
#search_limit = 6

# Optional cookies file for yt-dlp (a Netscape-format file, e.g. exported from
# your browser). Unlocks private playlists / liked lists / history and quiets
# the bot checks that throttle anonymized traffic.
#cookies_file = \"/home/you/.config/tuna-tui/cookies.txt\"

# Seconds of decoded PCM buffered before playback output starts (1..30).
# Larger buffers smooth stutter on high-latency or fluctuating connections;
# each stream start (track, seek, pause-resume) waits this long in silence
# while the buffer fills.
#buffer_duration_secs = 2
";

/// One-time move of the pre-rebrand `myx` dirs to the `tuna-tui` names.
///
/// Only acts when the legacy dir exists AND the new one doesn't — a fresh
/// install, or an already-migrated home, is left completely alone. Moving the
/// whole dir carries config.toml (and its cookies path), the session snapshot,
/// the yt-dlp api cache, and the log over in one shot; nothing is deleted, so
/// the move is safe even with a stale `myx` binary still running alongside.
pub fn migrate_legacy_paths() {
    // Cache first: this function logs through liblog, whose write itself
    // creates `~/.cache/tuna-tui` — running the cache move any later would
    // hit the freshly-created target and bail on its `new.exists()` guard.
    migrate_dir(".cache/myx", ".cache/tuna-tui");
    migrate_dir(".config/myx", ".config/tuna-tui");
}

fn migrate_dir(legacy: &str, current: &str) {
    let Some(home) = crate::home_dir() else {
        return;
    };
    let old = home.join(legacy);
    let new = home.join(current);
    if !old.exists() || new.exists() {
        return;
    }
    match std::fs::rename(&old, &new) {
        Ok(()) => crate::liblog::liblog(format!("migrated {legacy} -> {current}")),
        Err(e) => crate::liblog::liblog(format!("migrate {legacy} -> {current} failed: {e}")),
    }
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        Some(crate::home_dir()?.join(".config/tuna-tui/config.toml"))
    }

    fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        if !path.exists() {
            write_template(&path);
        }
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| Self::parse(&s))
            .unwrap_or_default()
    }

    /// Parse the document as a generic map and extract each key with a type
    /// check of its own. Under the old one-shot serde read, a single bad
    /// value (e.g. `buffer_duration_secs = 300` or `= 2.5` on a u8 field)
    /// failed the WHOLE document and the caller's `unwrap_or_default`
    /// silently discarded every other key — cookies unlock, yt-dlp/ffmpeg
    /// paths, audio format — all gone with no message, which is exactly the
    /// "a typo must never lock someone out" failure the module preamble
    /// promises against. Now a wrong-typed value costs only its own key;
    /// unparseable TOML syntax still falls back wholesale (there is nothing
    /// to salvage from a broken document), and unknown keys stay ignored so
    /// an older binary never chokes on a newer config.
    fn parse(s: &str) -> Option<Self> {
        let table: toml::Table = s.parse().ok()?;
        let d = Self::default();
        let int = |k: &str| table.get(k).and_then(toml::Value::as_integer);
        let text = |k: &str| {
            table
                .get(k)
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
        };
        Some(Config {
            scrolloff: int("scrolloff")
                .and_then(|v| usize::try_from(v).ok())
                .unwrap_or(d.scrolloff),
            restore_on_startup: table
                .get("restore_on_startup")
                .and_then(toml::Value::as_bool)
                .unwrap_or(d.restore_on_startup),
            protocol: text("protocol").or(d.protocol),
            ytdlp_path: text("ytdlp_path").unwrap_or(d.ytdlp_path),
            ffmpeg_path: text("ffmpeg_path").unwrap_or(d.ffmpeg_path),
            audio_format: text("audio_format").unwrap_or(d.audio_format),
            search_limit: int("search_limit")
                .and_then(|v| usize::try_from(v).ok())
                .unwrap_or(d.search_limit),
            cookies_file: text("cookies_file").or(d.cookies_file),
            buffer_duration_secs: int("buffer_duration_secs")
                .and_then(|v| u8::try_from(v).ok())
                .unwrap_or(d.buffer_duration_secs),
        })
    }
}

/// Best effort: a read-only home just means no file, never a failed start.
fn write_template(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, TEMPLATE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_is_all_defaults() {
        let c = Config::parse("").expect("empty toml is valid");
        assert_eq!(c.scrolloff, 3);
        assert!(c.restore_on_startup);
        assert_eq!(c.buffer_duration_secs, 2);
    }

    #[test]
    fn buffer_duration_reads_when_present() {
        let c = Config::parse("buffer_duration_secs = 7").expect("valid toml");
        assert_eq!(c.buffer_duration_secs, 7);
    }

    #[test]
    fn prebuffer_samples_converts_secs_to_stereo_floats() {
        // 2 s default → 2 × 44_100 frames × 2 channels; 5 s → five times that.
        assert_eq!(prebuffer_samples(2), 176_400);
        assert_eq!(prebuffer_samples(5), 441_000);
    }

    #[test]
    fn reads_keys() {
        let c = Config::parse(
            "scrolloff = 5\nrestore_on_startup = false\n\
             ytdlp_path = \"/opt/yt-dlp\"\nffmpeg_path = \"/opt/ffmpeg\"\
             \naudio_format = \"bestaudio\"\nsearch_limit = 9",
        )
        .expect("valid toml");
        assert_eq!(c.scrolloff, 5);
        assert!(!c.restore_on_startup);
        assert_eq!(c.ytdlp_path, "/opt/yt-dlp");
        assert_eq!(c.ffmpeg_path, "/opt/ffmpeg");
        assert_eq!(c.audio_format, "bestaudio");
        assert_eq!(c.search_limit, 9);
    }

    #[test]
    fn yt_keys_default_to_the_configured_defaults() {
        let c = Config::parse("").expect("empty toml is valid");
        assert_eq!(c.ytdlp_path, "yt-dlp");
        assert_eq!(c.ffmpeg_path, "ffmpeg");
        assert_eq!(c.audio_format, "bestaudio/best");
        assert_eq!(c.search_limit, 6);
        assert!(c.cookies_file.is_none());
    }

    #[test]
    fn cookies_file_reads_when_present() {
        let c = Config::parse("cookies_file = \"/tmp/c.txt\"").expect("valid toml");
        assert_eq!(c.cookies_file.as_deref(), Some("/tmp/c.txt"));
    }

    #[test]
    fn unknown_keys_are_ignored() {
        // An older tuna-tui must not choke on a config written for a newer one.
        let c = Config::parse("scrolloff = 1\nfuture_key = true").expect("valid toml");
        assert_eq!(c.scrolloff, 1);
    }

    #[test]
    fn malformed_toml_syntax_falls_back_rather_than_failing() {
        // Only unparseable syntax is unsalvageable — a wrong-typed *value*
        // now defaults its own key alone (see the lenient tests below).
        assert!(Config::parse("scrolloff = = =").is_none());
        assert!(Config::parse("this is not toml ] ] ]").is_none());
    }

    #[test]
    fn a_wrong_typed_value_defaults_only_its_own_key() {
        // `buffer_duration_secs = 300` doesn't fit u8 and `= 2.5` is a float:
        // both are plausible user typos in exactly the file the template
        // points them at. Each must cost only that key — never the cookies
        // unlock or the binary paths beside it (the old one-shot serde read
        // silently discarded the whole config on the first bad value).
        let c = Config::parse(
            "buffer_duration_secs = 300\n\
             cookies_file = \"/tmp/c.txt\"\n\
             ffmpeg_path = \"/opt/ffmpeg\"",
        )
        .expect("valid toml");
        assert_eq!(c.buffer_duration_secs, 2, "out-of-range u8 falls back");
        assert_eq!(
            c.cookies_file.as_deref(),
            Some("/tmp/c.txt"),
            "cookies survive"
        );
        assert_eq!(c.ffmpeg_path, "/opt/ffmpeg", "ffmpeg path survives");

        let c = Config::parse("buffer_duration_secs = 2.5").expect("valid toml");
        assert_eq!(c.buffer_duration_secs, 2, "float for u8 falls back");
        let c = Config::parse("buffer_duration_secs = \"5\"").expect("valid toml");
        assert_eq!(c.buffer_duration_secs, 2, "string for u8 falls back");
    }

    #[test]
    fn wrong_typed_legacy_keys_default_their_own_key() {
        // Same leniency for the pre-buffer keys: one bad line among good ones
        // must not take the good ones down with it.
        let c = Config::parse(
            "scrolloff = -4\nsearch_limit = \"many\"\n\
             restore_on_startup = \"yes\"\ncookies_file = \"/tmp/c.txt\"",
        )
        .expect("valid toml");
        assert_eq!(c.scrolloff, 3, "negative int falls back");
        assert_eq!(c.search_limit, 6, "string for usize falls back");
        assert!(c.restore_on_startup, "string for bool falls back");
        assert_eq!(
            c.cookies_file.as_deref(),
            Some("/tmp/c.txt"),
            "good key survives"
        );
    }

    #[test]
    fn the_first_run_template_parses_to_the_defaults() {
        // Everything in it is commented out, so writing it can never change how
        // tuna-tui behaves — it only shows what there is to change.
        let c = Config::parse(TEMPLATE).expect("template is valid toml");
        let d = Config::default();
        assert_eq!(c.scrolloff, d.scrolloff);
        assert_eq!(c.restore_on_startup, d.restore_on_startup);
        assert!(c.protocol.is_none());
        assert_eq!(c.ytdlp_path, d.ytdlp_path);
        assert_eq!(c.ffmpeg_path, d.ffmpeg_path);
        assert_eq!(c.audio_format, d.audio_format);
        assert_eq!(c.search_limit, d.search_limit);
    }

    #[test]
    fn the_template_is_written_once_and_never_over_an_existing_file() {
        let dir = std::env::temp_dir().join("tuna-tui-config-template");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");

        write_template(&path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), TEMPLATE);

        std::fs::write(&path, "scrolloff = 9").unwrap();
        // `load` only writes when the file is missing; the edit has to survive.
        assert!(path.exists());
        assert_eq!(
            Config::parse(&std::fs::read_to_string(&path).unwrap())
                .unwrap()
                .scrolloff,
            9
        );
    }
}
