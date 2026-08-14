//! User settings from `~/.config/tuna-tui/config.toml`. Missing, empty or
//! malformed all fall back to defaults — a typo must never lock someone out of
//! the app, and a wrong-typed *value* must never take out the keys beside it
//! (see [`Config::parse`]).
//!
//! A value that fails its key's type check defaults that key alone; a line
//! that fails TOML *syntax* (an out-of-i64-range integer literal is one) is
//! dropped and the rest salvaged, so it costs only its own key too. Only a
//! document whose badness spans lines falls back wholesale.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VisualizerStyle {
    #[default]
    Block,
    Braille,
    Line,
    Solid,
}

impl VisualizerStyle {
    pub const ALL: [VisualizerStyle; 4] = [
        VisualizerStyle::Block,
        VisualizerStyle::Braille,
        VisualizerStyle::Line,
        VisualizerStyle::Solid,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Block => "Block ( ▂▃▄▅▆▇█)",
            Self::Braille => "Braille (⠁⠃⠇⡇⣇⣧⣷⣿)",
            Self::Line => "Line (─)",
            Self::Solid => "Solid (█)",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Braille => "braille",
            Self::Line => "line",
            Self::Solid => "solid",
        }
    }

    pub fn parse_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "block" => Some(Self::Block),
            "braille" => Some(Self::Braille),
            "line" => Some(Self::Line),
            "solid" => Some(Self::Solid),
            _ => None,
        }
    }
}

impl std::str::FromStr for VisualizerStyle {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_str(s).ok_or(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VisualizerSmoothing {
    Snappy,
    #[default]
    Balanced,
    Liquid,
}

impl VisualizerSmoothing {
    pub const ALL: [VisualizerSmoothing; 3] = [
        VisualizerSmoothing::Snappy,
        VisualizerSmoothing::Balanced,
        VisualizerSmoothing::Liquid,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Snappy => "Snappy",
            Self::Balanced => "Balanced",
            Self::Liquid => "Liquid",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Snappy => "snappy",
            Self::Balanced => "balanced",
            Self::Liquid => "liquid",
        }
    }

    pub fn parse_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "snappy" => Some(Self::Snappy),
            "balanced" => Some(Self::Balanced),
            "liquid" => Some(Self::Liquid),
            _ => None,
        }
    }
}

impl std::str::FromStr for VisualizerSmoothing {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_str(s).ok_or(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioQuality {
    #[default]
    Best,
    High,
    DataSaver,
}

impl AudioQuality {
    pub const ALL: [AudioQuality; 3] = [
        AudioQuality::Best,
        AudioQuality::High,
        AudioQuality::DataSaver,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Best => "Best Available",
            Self::High => "High (Opus 160k)",
            Self::DataSaver => "Data Saver (64k)",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Best => "best",
            Self::High => "high",
            Self::DataSaver => "data_saver",
        }
    }

    pub fn parse_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "best" => Some(Self::Best),
            "high" => Some(Self::High),
            "data_saver" | "datasaver" | "low" => Some(Self::DataSaver),
            _ => None,
        }
    }
}

impl std::str::FromStr for AudioQuality {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_str(s).ok_or(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LyricsAlignment {
    #[default]
    Center,
    Left,
    Right,
}

impl LyricsAlignment {
    pub const ALL: [LyricsAlignment; 3] = [
        LyricsAlignment::Center,
        LyricsAlignment::Left,
        LyricsAlignment::Right,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Center => "Center",
            Self::Left => "Left",
            Self::Right => "Right",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Center => "center",
            Self::Left => "left",
            Self::Right => "right",
        }
    }

    pub fn parse_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "center" => Some(Self::Center),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            _ => None,
        }
    }
}

impl std::str::FromStr for LyricsAlignment {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_str(s).ok_or(())
    }
}

/// Stereo f32 samples for `secs` of 44.1 kHz audio — what the engine's
/// prebuffer gate counts.
pub fn prebuffer_samples(secs: u8) -> usize {
    secs as usize * 44_100 * 2
}

#[derive(Clone, Debug, PartialEq, Eq)]
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

    // --- Visuals & Motion ---
    /// Animation / Visualizer target FPS (e.g. 30, 60, 120, 240, 1000).
    pub animation_fps: u16,
    /// Character rendering style for the spectrum visualizer.
    pub visualizer_style: VisualizerStyle,
    /// Smoothing filter applied to visualizer frequency bands.
    pub visualizer_smoothing: VisualizerSmoothing,
    /// Width of each spectrum bar in terminal cells (1..4).
    pub visualizer_bar_width: u8,
    /// Color gradient scheme for visualizer (0=default, 1=fire, 2=ocean, 3=forest, 4=sunset, 5=mono).
    pub visualizer_color_scheme: u8,
    /// Progress bar glyph style (0=blocks, 1=braille, 2=line, 3=gradient, 4=dual).
    pub progress_bar_style: u8,
    /// Theme transition cross-fade duration in milliseconds.
    pub theme_fade_speed: u16,
    /// Default to Zen mode (fullscreen Now Playing without left sidebar).
    pub zen_default: bool,
    /// Active theme name ("Adaptive", "Tokyo Night", "Catppuccin", etc.).
    pub theme_name: String,

    // --- Playback & Audio ---
    /// Audio stream quality preference.
    pub audio_quality: AudioQuality,
    /// Volume increment/decrement step percentage (1..=25).
    pub volume_step: u8,
    /// Enable crossfade between tracks.
    pub crossfade_enabled: bool,
    /// Crossfade overlap duration in milliseconds.
    pub crossfade_duration_ms: u16,
    /// Enable gapless playback for consecutive tracks.
    pub gapless_playback: bool,
    /// Enable ReplayGain loudness normalization.
    pub replay_gain: bool,
    /// Resolve next track stream URL in advance.
    pub next_track_prefetch: bool,

    // --- Lyrics ---
    /// Text alignment in the lyrics view.
    pub lyrics_alignment: LyricsAlignment,
    /// Whether to auto-transliterate non-Latin (Indic/CJK) lyrics and metadata.
    pub lyrics_transliterate: bool,
    /// Auto-scroll synced lyrics to keep active verse centered.
    pub lyrics_auto_scroll: bool,
    /// Relative font size for lyrics (80..150%).
    pub lyrics_font_size: u8,
    /// Highlight style for active verse (0=bold+primary, 1=bold+accent, 2=inverted, 3=underline, 4=block).
    pub lyrics_highlight_color: u8,

    // --- Interface ---
    /// UI density: 0=comfortable, 1=standard, 2=compact.
    pub ui_density: u8,
    /// Show track thumbnails in Queue view.
    pub show_album_art_in_queue: bool,
    /// Show progress bar and time in footer.
    pub show_progress_in_footer: bool,
    /// Footer clock format: 0=12h, 1=24h, 2=relative, 3=hidden.
    pub footer_clock_format: u8,
    /// Sidebar width as percentage of terminal width (20..50).
    pub sidebar_width_pct: u8,

    // --- System ---
    /// Enable MPRIS / system media control integration.
    pub mpris_enabled: bool,

    // --- Debug & Diagnostics ---
    /// Master toggle for all debug subsystems.
    pub debug_mode: bool,
    /// Verbose logging of engine events, metadata fetches, state transitions.
    pub debug_verbose_logging: bool,
    /// Real-time performance overlay (FPS, frame time, CPU, buffer health).
    pub debug_performance_overlay: bool,
    /// Network request/response logging with timing.
    pub debug_network_logging: bool,
    /// Audio decoder, buffer, underrun, sample rate logging.
    pub debug_audio_diagnostics: bool,
    /// Engine queue, track loading, shuffle state logging.
    pub debug_engine_state: bool,
    /// Raw FFT magnitudes, band values, peak envelope per frame.
    pub debug_visualizer_raw: bool,
    /// Metadata cache hits/misses, eviction, LRU order logging.
    pub debug_cache_stats: bool,
    /// Lyrics sync parsing, line matching, scroll drift logging.
    pub debug_lyrics_timing: bool,
    /// Search query, suggestion, result count, latency logging.
    pub debug_search_queries: bool,
    /// Write debug output to rotating file (~/.cache/tuna-tui/debug.log).
    pub debug_log_file: bool,
    /// Debug log verbosity: 0=Error, 1=Warn, 2=Info, 3=Debug, 4=Trace.
    pub debug_log_level: u8,
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

            animation_fps: 120,
            visualizer_style: VisualizerStyle::Block,
            visualizer_smoothing: VisualizerSmoothing::Balanced,
            visualizer_bar_width: 1,
            visualizer_color_scheme: 0,
            progress_bar_style: 0,
            theme_fade_speed: 1500,
            zen_default: false,
            theme_name: "Adaptive".to_string(),

            audio_quality: AudioQuality::Best,
            volume_step: 5,
            crossfade_enabled: false,
            crossfade_duration_ms: 3000,
            gapless_playback: true,
            replay_gain: false,
            next_track_prefetch: true,

            lyrics_alignment: LyricsAlignment::Center,
            lyrics_transliterate: true,
            lyrics_auto_scroll: true,
            lyrics_font_size: 100,
            lyrics_highlight_color: 0,

            ui_density: 1,
            show_album_art_in_queue: false,
            show_progress_in_footer: true,
            footer_clock_format: 1,
            sidebar_width_pct: 30,

            mpris_enabled: true,

            debug_mode: false,
            debug_verbose_logging: false,
            debug_performance_overlay: false,
            debug_network_logging: false,
            debug_audio_diagnostics: false,
            debug_engine_state: false,
            debug_visualizer_raw: false,
            debug_cache_stats: false,
            debug_lyrics_timing: false,
            debug_search_queries: false,
            debug_log_file: false,
            debug_log_level: 0,
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
    /// promises against. Now a wrong-typed value costs only its own key, an
    /// out-of-range integer literal costs only its own *line* (see the
    /// salvage pass), and unknown keys stay ignored so an older binary
    /// never chokes on a newer config.
    fn parse(s: &str) -> Option<Self> {
        // Fast path: the whole document parses.
        if let Ok(table) = s.parse::<toml::Table>() {
            return Some(Self::from_table(&table));
        }
        // Salvage: TOML integers are i64-bounded, so an over-range literal
        // (`buffer_duration_secs = 99999999999999999999`, issue #15) is a
        // DOCUMENT-level syntax error — the per-key reader above never sees
        // it. Drop one line at a time and retry: the first parseable
        // candidate is the document with the offending line removed, and
        // that key falls back to its default while every other key
        // survives. A document whose badness spans lines (a multi-line
        // construct broken twice) defeats single-line salvage and falls
        // back wholesale.
        for drop in 0..s.lines().count() {
            let mut candidate = String::with_capacity(s.len());
            for (i, line) in s.lines().enumerate() {
                if i != drop {
                    candidate.push_str(line);
                    candidate.push('\n');
                }
            }
            if let Ok(table) = candidate.parse::<toml::Table>() {
                return Some(Self::from_table(&table));
            }
        }
        None
    }

    /// The per-key lenient reader: extract each field with a type check of
    /// its own, defaulting per key. Shared by the fast path and the salvage
    /// candidates.
    fn from_table(table: &toml::Table) -> Self {
        let d = Self::default();
        let get_val = |k: &str| -> Option<&toml::Value> {
            table.get(k).or_else(|| {
                for section in ["display", "audio", "lyrics", "search", "system"] {
                    if let Some(v) = table.get(section).and_then(|t| t.as_table()).and_then(|t| t.get(k)) {
                        return Some(v);
                    }
                }
                None
            })
        };
        let int = |k: &str| get_val(k).and_then(toml::Value::as_integer);
        let boolean = |k: &str| get_val(k).and_then(toml::Value::as_bool);
        let text = |k: &str| {
            get_val(k)
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
        };
        Config {
            scrolloff: int("scrolloff")
                .and_then(|v| usize::try_from(v).ok())
                .unwrap_or(d.scrolloff),
            restore_on_startup: boolean("restore_on_startup").unwrap_or(d.restore_on_startup),
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
                .filter(|v| (1..=30).contains(v))
                .unwrap_or(d.buffer_duration_secs),

            animation_fps: int("animation_fps")
                .and_then(|v| u16::try_from(v).ok())
                .filter(|v| *v >= 1 && *v <= 1000)
                .unwrap_or(d.animation_fps),
            visualizer_style: text("visualizer_style")
                .and_then(|s| VisualizerStyle::parse_str(&s))
                .unwrap_or(d.visualizer_style),
            visualizer_smoothing: text("visualizer_smoothing")
                .and_then(|s| VisualizerSmoothing::parse_str(&s))
                .unwrap_or(d.visualizer_smoothing),
            visualizer_bar_width: int("visualizer_bar_width")
                .and_then(|v| u8::try_from(v).ok())
                .filter(|v| (1..=4).contains(v))
                .unwrap_or(d.visualizer_bar_width),
            visualizer_color_scheme: int("visualizer_color_scheme")
                .and_then(|v| u8::try_from(v).ok())
                .filter(|v| *v <= 5)
                .unwrap_or(d.visualizer_color_scheme),
            progress_bar_style: int("progress_bar_style")
                .and_then(|v| u8::try_from(v).ok())
                .filter(|v| *v <= 4)
                .unwrap_or(d.progress_bar_style),
            theme_fade_speed: int("theme_fade_speed")
                .and_then(|v| u16::try_from(v).ok())
                .filter(|v| *v >= 200 && *v <= 5000)
                .unwrap_or(d.theme_fade_speed),
            zen_default: boolean("zen_default").unwrap_or(d.zen_default),
            theme_name: text("theme_name").unwrap_or(d.theme_name),

            audio_quality: text("audio_quality")
                .and_then(|s| AudioQuality::parse_str(&s))
                .unwrap_or(d.audio_quality),
            volume_step: int("volume_step")
                .and_then(|v| u8::try_from(v).ok())
                .filter(|v| (1..=50).contains(v))
                .unwrap_or(d.volume_step),
            crossfade_enabled: boolean("crossfade_enabled").unwrap_or(d.crossfade_enabled),
            crossfade_duration_ms: int("crossfade_duration_ms")
                .and_then(|v| u16::try_from(v).ok())
                .filter(|v| *v >= 100 && *v <= 20000)
                .unwrap_or(d.crossfade_duration_ms),
            gapless_playback: boolean("gapless_playback").unwrap_or(d.gapless_playback),
            replay_gain: boolean("replay_gain").unwrap_or(d.replay_gain),
            next_track_prefetch: boolean("next_track_prefetch").unwrap_or(d.next_track_prefetch),

            lyrics_alignment: text("lyrics_alignment")
                .and_then(|s| LyricsAlignment::parse_str(&s))
                .unwrap_or(d.lyrics_alignment),
            lyrics_transliterate: boolean("lyrics_transliterate").unwrap_or(d.lyrics_transliterate),
            lyrics_auto_scroll: boolean("lyrics_auto_scroll").unwrap_or(d.lyrics_auto_scroll),
            lyrics_font_size: int("lyrics_font_size")
                .and_then(|v| u8::try_from(v).ok())
                .filter(|v| (50..=200).contains(v))
                .unwrap_or(d.lyrics_font_size),
            lyrics_highlight_color: int("lyrics_highlight_color")
                .and_then(|v| u8::try_from(v).ok())
                .filter(|v| *v <= 4)
                .unwrap_or(d.lyrics_highlight_color),

            ui_density: int("ui_density")
                .and_then(|v| u8::try_from(v).ok())
                .filter(|v| *v <= 2)
                .unwrap_or(d.ui_density),
            show_album_art_in_queue: boolean("show_album_art_in_queue").unwrap_or(d.show_album_art_in_queue),
            show_progress_in_footer: boolean("show_progress_in_footer").unwrap_or(d.show_progress_in_footer),
            footer_clock_format: int("footer_clock_format")
                .and_then(|v| u8::try_from(v).ok())
                .filter(|v| *v <= 3)
                .unwrap_or(d.footer_clock_format),
            sidebar_width_pct: int("sidebar_width_pct")
                .and_then(|v| u8::try_from(v).ok())
                .filter(|v| (10..=60).contains(v))
                .unwrap_or(d.sidebar_width_pct),

            mpris_enabled: boolean("mpris_enabled").unwrap_or(d.mpris_enabled),

            debug_mode: boolean("debug_mode").unwrap_or(d.debug_mode),
            debug_verbose_logging: boolean("debug_verbose_logging").unwrap_or(d.debug_verbose_logging),
            debug_performance_overlay: boolean("debug_performance_overlay").unwrap_or(d.debug_performance_overlay),
            debug_network_logging: boolean("debug_network_logging").unwrap_or(d.debug_network_logging),
            debug_audio_diagnostics: boolean("debug_audio_diagnostics").unwrap_or(d.debug_audio_diagnostics),
            debug_engine_state: boolean("debug_engine_state").unwrap_or(d.debug_engine_state),
            debug_visualizer_raw: boolean("debug_visualizer_raw").unwrap_or(d.debug_visualizer_raw),
            debug_cache_stats: boolean("debug_cache_stats").unwrap_or(d.debug_cache_stats),
            debug_lyrics_timing: boolean("debug_lyrics_timing").unwrap_or(d.debug_lyrics_timing),
            debug_search_queries: boolean("debug_search_queries").unwrap_or(d.debug_search_queries),
            debug_log_file: boolean("debug_log_file").unwrap_or(d.debug_log_file),
            debug_log_level: int("debug_log_level")
                .and_then(|v| u8::try_from(v).ok())
                .filter(|v| *v <= 4)
                .unwrap_or(d.debug_log_level),
        }
    }

    /// Serialize current configuration into clean, human-readable TOML.
    pub fn serialize_toml(&self) -> String {
        let mut out = String::new();
        out.push_str("# tuna-tui configuration\n\n");

        out.push_str("[display]\n");
        out.push_str(&format!("animation_fps = {}\n", self.animation_fps));
        out.push_str(&format!("visualizer_style = \"{}\"\n", self.visualizer_style.as_str()));
        out.push_str(&format!("visualizer_smoothing = \"{}\"\n", self.visualizer_smoothing.as_str()));
        out.push_str(&format!("visualizer_bar_width = {}\n", self.visualizer_bar_width));
        out.push_str(&format!("visualizer_color_scheme = {}\n", self.visualizer_color_scheme));
        out.push_str(&format!("progress_bar_style = {}\n", self.progress_bar_style));
        out.push_str(&format!("theme_fade_speed = {}\n", self.theme_fade_speed));
        if let Some(proto) = &self.protocol {
            out.push_str(&format!("protocol = \"{}\"\n", proto));
        }
        out.push_str(&format!("zen_default = {}\n", self.zen_default));
        out.push_str(&format!("theme_name = \"{}\"\n\n", self.theme_name));

        out.push_str("[audio]\n");
        out.push_str(&format!("audio_quality = \"{}\"\n", self.audio_quality.as_str()));
        out.push_str(&format!("buffer_duration_secs = {}\n", self.buffer_duration_secs));
        out.push_str(&format!("volume_step = {}\n", self.volume_step));
        out.push_str(&format!("crossfade_enabled = {}\n", self.crossfade_enabled));
        out.push_str(&format!("crossfade_duration_ms = {}\n", self.crossfade_duration_ms));
        out.push_str(&format!("gapless_playback = {}\n", self.gapless_playback));
        out.push_str(&format!("replay_gain = {}\n", self.replay_gain));
        out.push_str(&format!("restore_on_startup = {}\n", self.restore_on_startup));
