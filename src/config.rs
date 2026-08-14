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
