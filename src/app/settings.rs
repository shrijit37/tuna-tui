//! Settings state model, tabs, and interactive option descriptors.

use tuna_tui::config::{
    AudioQuality, Config, LyricsAlignment, VisualizerSmoothing, VisualizerStyle,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsTab {
    #[default]
    Visuals,
    Playback,
    Lyrics,
    Interface,
    System,
}

impl SettingsTab {
    pub const ALL: [SettingsTab; 5] = [
        SettingsTab::Visuals,
        SettingsTab::Playback,
        SettingsTab::Lyrics,
        SettingsTab::Interface,
        SettingsTab::System,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Visuals => "Visuals & Motion",
            Self::Playback => "Playback & Audio",
            Self::Lyrics => "Lyrics",
            Self::Interface => "Interface",
            Self::System => "System & Cache",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingControl {
    Toggle(bool),
    Choice {
        current: usize,
        options: Vec<String>,
    },
    Number {
        val: i64,
        min: i64,
        max: i64,
        step: i64,
        suffix: &'static str,
    },
    Action(&'static str),
    Separator(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingRow {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub control: SettingControl,
}

pub enum SettingsAction {
    ClearCache,
}

#[derive(Debug, Clone)]
pub struct SettingsState {
    pub tab: SettingsTab,
    pub selected: usize,
    pub dirty: bool,

    // Visuals & Motion
    pub animation_fps: u16,
    pub visualizer_style: VisualizerStyle,
    pub visualizer_smoothing: VisualizerSmoothing,
    pub visualizer_bar_width: u8,
    pub visualizer_color_scheme: u8,
    pub progress_bar_style: u8,
    pub protocol: Option<String>,
    pub zen_default: bool,
    pub theme_name: String,
    pub theme_fade_speed: u16,
    // Playback & Audio
    pub audio_quality: AudioQuality,
    pub buffer_duration_secs: u8,
    pub volume_step: u8,
    pub crossfade_enabled: bool,
    pub crossfade_duration_ms: u16,
    pub gapless_playback: bool,
    pub restore_on_startup: bool,
    pub next_track_prefetch: bool,
    pub replay_gain: bool,

    // Lyrics
    pub lyrics_alignment: LyricsAlignment,
    pub lyrics_transliterate: bool,
    pub lyrics_auto_scroll: bool,
    pub lyrics_font_size: u8,
    pub lyrics_highlight_color: u8,

    // Interface
    pub search_limit: usize,
    pub scrolloff: usize,
    pub ui_density: u8,
    pub show_album_art_in_queue: bool,
    pub show_progress_in_footer: bool,
    pub footer_clock_format: u8,
    pub sidebar_width_pct: u8,
    // System
    pub mpris_enabled: bool,

    // Debug & Diagnostics
    pub debug_mode: bool,
    pub debug_verbose_logging: bool,
    pub debug_performance_overlay: bool,
    pub debug_network_logging: bool,
    pub debug_audio_diagnostics: bool,
    pub debug_engine_state: bool,
    pub debug_visualizer_raw: bool,
    pub debug_cache_stats: bool,
    pub debug_lyrics_timing: bool,
    pub debug_search_queries: bool,
    pub debug_log_file: bool,
    pub debug_log_level: u8,

    pub status_msg: Option<String>,
}

impl SettingsState {
    pub fn init_from_config(c: &Config) -> Self {
        Self {
            tab: SettingsTab::Visuals,
            selected: 0,
            dirty: false,

            // Visuals
            animation_fps: c.animation_fps,
            visualizer_style: c.visualizer_style,
            visualizer_smoothing: c.visualizer_smoothing,
            visualizer_bar_width: c.visualizer_bar_width,
            visualizer_color_scheme: c.visualizer_color_scheme,
            progress_bar_style: c.progress_bar_style,
            protocol: c.protocol.clone(),
            zen_default: c.zen_default,
            theme_name: c.theme_name.clone(),
            theme_fade_speed: c.theme_fade_speed,

            // Playback
            audio_quality: c.audio_quality,
            buffer_duration_secs: c.buffer_duration_secs,
            volume_step: c.volume_step,
            crossfade_enabled: c.crossfade_enabled,
            crossfade_duration_ms: c.crossfade_duration_ms,
            gapless_playback: c.gapless_playback,
            restore_on_startup: c.restore_on_startup,
            next_track_prefetch: c.next_track_prefetch,
            replay_gain: c.replay_gain,

            // Lyrics
            lyrics_alignment: c.lyrics_alignment,
            lyrics_transliterate: c.lyrics_transliterate,
            lyrics_auto_scroll: c.lyrics_auto_scroll,
            lyrics_font_size: c.lyrics_font_size,
            lyrics_highlight_color: c.lyrics_highlight_color,

            // Interface
            search_limit: c.search_limit,
            scrolloff: c.scrolloff,
            ui_density: c.ui_density,
            show_album_art_in_queue: c.show_album_art_in_queue,
            show_progress_in_footer: c.show_progress_in_footer,
            footer_clock_format: c.footer_clock_format,
            sidebar_width_pct: c.sidebar_width_pct,

            // System
            mpris_enabled: c.mpris_enabled,

            // Debug
            debug_mode: c.debug_mode,
            debug_verbose_logging: c.debug_verbose_logging,
            debug_performance_overlay: c.debug_performance_overlay,
            debug_network_logging: c.debug_network_logging,
            debug_audio_diagnostics: c.debug_audio_diagnostics,
            debug_engine_state: c.debug_engine_state,
            debug_visualizer_raw: c.debug_visualizer_raw,
            debug_cache_stats: c.debug_cache_stats,
            debug_lyrics_timing: c.debug_lyrics_timing,
            debug_search_queries: c.debug_search_queries,
            debug_log_file: c.debug_log_file,
            debug_log_level: c.debug_log_level,

            status_msg: None,
        }
    }

    pub fn rows_for_tab(&self, tab: SettingsTab) -> Vec<SettingRow> {
        match tab {
            SettingsTab::Visuals => vec![
                SettingRow {
                    id: "fps",
                    label: "Animation & Visualizer FPS",
                    description: "Target render frame rate for visualizer, lyrics scrolling, and theme transitions.",
                    control: SettingControl::Choice {
                        current: match self.animation_fps {
                            fps if fps <= 30 => 0,
                            fps if fps <= 60 => 1,
                            fps if fps <= 120 => 2,
                            fps if fps <= 240 => 3,
                            _ => 4,
                        },
                        options: vec![
                            "30 FPS (33ms) — Battery saver".into(),
                            "60 FPS (16ms) — Standard smooth".into(),
                            "120 FPS (8ms) — High refresh".into(),
                            "240 FPS (4ms) — Gaming tier".into(),
                            "1ms (Uncapped) — Maximum".into(),
                        ],
                    },
                },
                SettingRow {
                    id: "theme",
                    label: "UI Theme Palette",
                    description: "Complete color palette for all UI components. \"Adaptive\" extracts colors from album art.",
                    control: SettingControl::Choice {
                        current: match self.theme_name.to_lowercase().as_str() {
                            "tokyo night" | "tokyonight" => 1,
                            "catppuccin" | "catppuccin mocha" => 2,
                            "gruvbox" | "gruvbox dark" => 3,
                            "nord" => 4,
                            "rosé pine" | "rose pine" => 5,
                            "dracula" => 6,
                            "monokai" => 7,
                            "solarized" => 8,
                            _ => 0,
                        },
                        options: vec![
                            "Adaptive (Album Art Reactive)".into(),
                            "Tokyo Night".into(),
                            "Catppuccin Mocha".into(),
                            "Gruvbox Dark".into(),
                            "Nord".into(),
                            "Rosé Pine".into(),
                            "Dracula".into(),
                            "Monokai".into(),
                            "Solarized Dark".into(),
                        ],
                    },
                },
                SettingRow {
                    id: "theme_fade",
                    label: "Theme Transition Speed",
                    description: "How quickly colors cross-fade when track changes. Lower = snappier.",
                    control: SettingControl::Number {
                        val: self.theme_fade_speed as i64,
                        min: 200,
                        max: 3000,
                        step: 100,
                        suffix: " ms",
                    },
                },
                SettingRow {
                    id: "sep_viz",
                    label: "",
                    description: "",
                    control: SettingControl::Separator("━━━  Spectrum Visualizer  ━━━"),
                },
                SettingRow {
                    id: "viz_style",
                    label: "Visualizer Style",
                    description: "Character glyph set for the frequency spectrum bars.",
                    control: SettingControl::Choice {
                        current: VisualizerStyle::ALL
                            .iter()
                            .position(|s| *s == self.visualizer_style)
                            .unwrap_or(0),
                        options: VisualizerStyle::ALL.iter().map(|s| s.label().to_string()).collect(),
                    },
                },
                SettingRow {
                    id: "viz_smoothing",
                    label: "Smoothing Intensity",
                    description: "Spatial filter passes. Snappy = responsive, Liquid = fluid waves.",
                    control: SettingControl::Choice {
                        current: VisualizerSmoothing::ALL
                            .iter()
                            .position(|s| *s == self.visualizer_smoothing)
                            .unwrap_or(1),
                        options: VisualizerSmoothing::ALL.iter().map(|s| s.label().to_string()).collect(),
                    },
                },
                SettingRow {
                    id: "viz_bar_width",
                    label: "Bar Width",
                    description: "Width of each spectrum column in terminal cells.",
                    control: SettingControl::Number {
                        val: self.visualizer_bar_width as i64,
                        min: 1,
                        max: 4,
                        step: 1,
                        suffix: " cells",
                    },
                },
                SettingRow {
                    id: "viz_colors",
                    label: "Color Gradient",
                    description: "Gradient palette for spectrum height mapping.",
                    control: SettingControl::Choice {
                        current: self.visualizer_color_scheme as usize,
                        options: vec![
                            "Default (Info→Primary→Accent)".into(),
                            "Fire (Red→Orange→Yellow)".into(),
                            "Ocean (Cyan→Blue→Purple)".into(),
                            "Forest (Green→Teal→Emerald)".into(),
                            "Sunset (Magenta→Pink→Orange)".into(),
                            "Monochrome (White→Gray→Dim)".into(),
                        ],
                    },
                },
                SettingRow {
                    id: "progress_style",
                    label: "Progress Bar Style",
                    description: "Glyph and rendering style for the seek/progress bar.",
                    control: SettingControl::Choice {
                        current: self.progress_bar_style as usize,
                        options: vec![
                            "Solid Blocks (▬▬▬)".into(),
                            "Braille Dots (⠁⠃⠇)".into(),
                            "Thin Line (━━━)".into(),
                            "Gradient Blocks (░▒▓█)".into(),
                            "Dual-Tone (░▓)".into(),
                        ],
                    },
                },
                SettingRow {
                    id: "sep_art",
                    label: "",
                    description: "",
                    control: SettingControl::Separator("━━━  Album Art  ━━━"),
                },
                SettingRow {
                    id: "protocol",
                    label: "Graphics Protocol",
                    description: "Terminal image protocol. Auto-detect queries the terminal; force one if art appears as mosaic.",
                    control: SettingControl::Choice {
                        current: match self.protocol.as_deref() {
                            Some("kitty") => 1,
                            Some("sixel") => 2,
                            Some("iterm2") => 3,
                            Some("halfblocks") => 4,
                            _ => 0,
                        },
                        options: vec![
                            "Auto-detect".into(),
                            "Kitty (GPU-accelerated)".into(),
                            "Sixel (tmux-friendly)".into(),
                            "iTerm2 (native)".into(),
                            "Halfblocks (universal)".into(),
                        ],
                    },
                },
                SettingRow {
                    id: "zen",
                    label: "Default to Zen Mode",
                    description: "Launch in fullscreen Now Playing without the sidebar.",
                    control: SettingControl::Toggle(self.zen_default),
                },
            ],
            SettingsTab::Playback => vec![
                SettingRow {
                    id: "sep_quality",
                    label: "",
                    description: "",
                    control: SettingControl::Separator("━━━  Stream Quality  ━━━"),
                },
                SettingRow {
                    id: "audio_quality",
                    label: "Audio Stream Quality",
                    description: "YouTube audio format preference. Higher quality = more bandwidth.",
                    control: SettingControl::Choice {
                        current: AudioQuality::ALL
                            .iter()
                            .position(|q| *q == self.audio_quality)
                            .unwrap_or(0),
                        options: AudioQuality::ALL.iter().map(|q| q.label().to_string()).collect(),
                    },
                },
                SettingRow {
                    id: "buffer_duration",
                    label: "Prebuffer Duration",
                    description: "Seconds of decoded audio buffered before playback starts. Higher = fewer stutters on slow networks.",
                    control: SettingControl::Number {
                        val: self.buffer_duration_secs as i64,
                        min: 1,
                        max: 15,
                        step: 1,
                        suffix: " s",
                    },
                },
                SettingRow {
                    id: "sep_transition",
                    label: "",
                    description: "",
                    control: SettingControl::Separator("━━━  Track Transitions  ━━━"),
                },
                SettingRow {
                    id: "crossfade",
                    label: "Crossfade Between Tracks",
                    description: "Overlap end of current track with start of next for seamless flow.",
                    control: SettingControl::Toggle(self.crossfade_enabled),
                },
                SettingRow {
                    id: "crossfade_duration",
                    label: "Crossfade Duration",
                    description: "Overlap length in milliseconds. Only applies when crossfade is enabled.",
                    control: SettingControl::Number {
                        val: self.crossfade_duration_ms as i64,
                        min: 500,
                        max: 10000,
                        step: 250,
                        suffix: " ms",
                    },
                },
                SettingRow {
                    id: "gapless",
                    label: "Gapless Playback",
                    description: "Eliminate silence between consecutive tracks from the same album/playlist.",
                    control: SettingControl::Toggle(self.gapless_playback),
                },
                SettingRow {
                    id: "replay_gain",
                    label: "ReplayGain Normalization",
                    description: "Automatically normalize loudness across tracks using embedded tags.",
                    control: SettingControl::Toggle(self.replay_gain),
                },
                SettingRow {
                    id: "sep_volume",
                    label: "",
                    description: "",
                    control: SettingControl::Separator("━━━  Volume & Controls  ━━━"),
                },
                SettingRow {
                    id: "volume_step",
                    label: "Volume Step Size",
                    description: "Percentage change per +/- keypress or mouse scroll.",
                    control: SettingControl::Number {
                        val: self.volume_step as i64,
                        min: 1,
                        max: 25,
                        step: 1,
                        suffix: " %",
                    },
                },
                SettingRow {
                    id: "sep_startup",
                    label: "",
                    description: "",
                    control: SettingControl::Separator("━━━  Startup & Prefetching  ━━━"),
                },
                SettingRow {
                    id: "restore_on_startup",
                    label: "Resume Playback on Launch",
                    description: "Restore last track, queue position, and playback state.",
                    control: SettingControl::Toggle(self.restore_on_startup),
                },
                SettingRow {
                    id: "next_track_prefetch",
                    label: "Next-Track Audio Prefetching",
                    description: "Resolve next song stream URL in background for instant transitions.",
                    control: SettingControl::Toggle(self.next_track_prefetch),
                },
            ],
            SettingsTab::Lyrics => vec![
                SettingRow {
                    id: "sep_align",
                    label: "",
                    description: "",
                    control: SettingControl::Separator("━━━  Layout & Alignment  ━━━"),
                },
                SettingRow {
                    id: "lyrics_align",
                    label: "Lyrics Alignment",
                    description: "Horizontal text alignment for verses in the Lyrics view.",
                    control: SettingControl::Choice {
                        current: LyricsAlignment::ALL
                            .iter()
                            .position(|a| *a == self.lyrics_alignment)
                            .unwrap_or(0),
                        options: LyricsAlignment::ALL.iter().map(|a| a.label().to_string()).collect(),
                    },
                },
                SettingRow {
                    id: "lyrics_font_size",
                    label: "Lyrics Font Scale",
                    description: "Relative size of lyrics text. 100% = normal terminal size.",
                    control: SettingControl::Number {
                        val: self.lyrics_font_size as i64,
                        min: 80,
                        max: 150,
                        step: 10,
                        suffix: " %",
                    },
                },
                SettingRow {
                    id: "lyrics_highlight",
                    label: "Active Line Highlight",
                    description: "Visual style for the currently playing verse.",
                    control: SettingControl::Choice {
                        current: self.lyrics_highlight_color as usize,
                        options: vec![
                            "Bold + Primary Color".into(),
                            "Bold + Accent Color".into(),
                            "Inverted (Background Swap)".into(),
                            "Underline Only".into(),
                            "Background Block".into(),
                        ],
                    },
                },
                SettingRow {
                    id: "sep_behavior",
                    label: "",
                    description: "",
                    control: SettingControl::Separator("━━━  Scrolling & Behavior  ━━━"),
                },
                SettingRow {
                    id: "lyrics_auto_scroll",
                    label: "Auto-Scroll Synced Lyrics",
                    description: "Keep the active verse centered while lyrics scroll with playback.",
                    control: SettingControl::Toggle(self.lyrics_auto_scroll),
                },
                SettingRow {
                    id: "lyrics_transliterate",
                    label: "Auto-Transliteration",
                    description: "Convert non-Latin scripts (Indic, CJK, Arabic) to phonetic Latin.",
                    control: SettingControl::Toggle(self.lyrics_transliterate),
                },
            ],
            SettingsTab::Interface => vec![
                SettingRow {
                    id: "sep_search",
                    label: "",
                    description: "",
                    control: SettingControl::Separator("━━━  Search & Library  ━━━"),
                },
                SettingRow {
                    id: "search_limit",
                    label: "Search Result Limit",
                    description: "Maximum tracks returned per YouTube Music search query.",
                    control: SettingControl::Number {
                        val: self.search_limit as i64,
                        min: 5,
                        max: 50,
                        step: 5,
                        suffix: " tracks",
                    },
                },
                SettingRow {
                    id: "scrolloff",
                    label: "List Cursor Scrolloff",
                    description: "Visible rows kept above/below cursor (vim 'scrolloff').",
                    control: SettingControl::Number {
                        val: self.scrolloff as i64,
                        min: 0,
                        max: 15,
                        step: 1,
                        suffix: " rows",
                    },
                },
                SettingRow {
                    id: "ui_density",
                    label: "UI Density",
                    description: "Vertical spacing between list items. Compact = more items on screen.",
                    control: SettingControl::Choice {
                        current: self.ui_density as usize,
                        options: vec![
                            "Comfortable (2-line items)".into(),
                            "Standard (1-line items)".into(),
                            "Compact (tight packing)".into(),
                        ],
                    },
                },
                SettingRow {
                    id: "sep_layout",
                    label: "",
                    description: "",
                    control: SettingControl::Separator("━━━  Layout & Panels  ━━━"),
                },
                SettingRow {
                    id: "sidebar_width",
                    label: "Sidebar Width",
                    description: "Percentage of terminal width for the left library sidebar.",
                    control: SettingControl::Number {
                        val: self.sidebar_width_pct as i64,
                        min: 20,
                        max: 50,
                        step: 5,
                        suffix: " %",
                    },
                },
                SettingRow {
                    id: "show_art_queue",
                    label: "Album Art in Queue",
                    description: "Show track thumbnails in the Queue view (requires width).",
                    control: SettingControl::Toggle(self.show_album_art_in_queue),
                },
                SettingRow {
                    id: "show_progress_footer",
                    label: "Progress in Footer",
                    description: "Show mini progress bar and time in the bottom footer bar.",
                    control: SettingControl::Toggle(self.show_progress_in_footer),
                },
                SettingRow {
                    id: "footer_clock",
                    label: "Footer Clock Format",
                    description: "Time display style in the footer.",
                    control: SettingControl::Choice {
                        current: self.footer_clock_format as usize,
                        options: vec![
                            "12h (3:45 PM)".into(),
                            "24h (15:45)".into(),
                            "Relative (3m 45s)".into(),
                            "Hidden".into(),
                        ],
                    },
                },
                SettingRow {
                    id: "sep_behavior_ui",
                    label: "",
                    description: "",
                    control: SettingControl::Separator("━━━  Behavior  ━━━"),
                },
            ],
            SettingsTab::System => vec![
                SettingRow {
                    id: "sep_system",
                    label: "",
                    description: "",
                    control: SettingControl::Separator("━━━  System Integration  ━━━"),
                },
                SettingRow {
                    id: "mpris",
                    label: "MPRIS / Media Keys",
                    description: "Enable OS media controls, keyboard media keys, and playerctl.",
                    control: SettingControl::Toggle(self.mpris_enabled),
                },
                SettingRow {
                    id: "sep_debug",
                    label: "",
                    description: "",
                    control: SettingControl::Separator("━━━  Debug & Diagnostics  ━━━"),
                },
                SettingRow {
                    id: "debug_mode",
                    label: "Debug Mode (Master Toggle)",
                    description: "Enable all debug subsystems. When on, individual toggles below take effect.",
                    control: SettingControl::Toggle(self.debug_mode),
                },
                SettingRow {
                    id: "debug_verbose",
                    label: "Verbose Logging",
                    description: "Log every engine event, metadata fetch, and state transition to console/file.",
                    control: SettingControl::Toggle(self.debug_verbose_logging),
                },
                SettingRow {
                    id: "debug_perf",
                    label: "Performance Overlay",
                    description: "Show real-time FPS, frame time, CPU/memory, audio buffer health in corner.",
                    control: SettingControl::Toggle(self.debug_performance_overlay),
                },
                SettingRow {
                    id: "debug_network",
                    label: "Network Request Logging",
                    description: "Log all HTTP requests/responses (YouTube, LRCLIB, metadata) with timing.",
                    control: SettingControl::Toggle(self.debug_network_logging),
                },
                SettingRow {
                    id: "debug_audio",
                    label: "Audio Pipeline Diagnostics",
                    description: "Log decoder state, buffer levels, underruns, sample rate changes, seek operations.",
                    control: SettingControl::Toggle(self.debug_audio_diagnostics),
                },
                SettingRow {
                    id: "debug_engine",
                    label: "Engine State Inspection",
                    description: "Log queue transitions, track loading, shuffle/repeat state, radio station switches.",
                    control: SettingControl::Toggle(self.debug_engine_state),
                },
                SettingRow {
                    id: "debug_viz",
                    label: "Visualizer Raw Data",
                    description: "Dump raw FFT magnitudes, band values, peak envelope per frame to log.",
                    control: SettingControl::Toggle(self.debug_visualizer_raw),
                },
                SettingRow {
                    id: "debug_cache",
                    label: "Cache Statistics",
                    description: "Log metadata cache hits/misses, eviction policy, LRU order, size on every access.",
                    control: SettingControl::Toggle(self.debug_cache_stats),
                },
                SettingRow {
