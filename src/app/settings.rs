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
