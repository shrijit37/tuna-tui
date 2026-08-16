//! User settings from `~/.config/tuna-tui/config.toml`. Missing, empty or malformed
//! all fall back to defaults — a typo must never lock someone out of the app.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Deserialize)]
#[serde(default)]
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

    fn parse(s: &str) -> Option<Self> {
        toml::from_str(s).ok()
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
    fn malformed_config_falls_back_rather_than_failing() {
        assert!(Config::parse("scrolloff = \"three\"").is_none());
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
