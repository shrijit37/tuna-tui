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
                    id: "sep_cache",
                    label: "",
                    description: "",
                    control: SettingControl::Separator("━━━  Cache Management  ━━━"),
                },
                SettingRow {
                    id: "clear_cache",
                    label: "Clear All Caches",
                    description: "Purge cached lyrics, album artwork, and API responses from ~/.cache/tuna-tui/.",
                    control: SettingControl::Action("Clear Now"),
                },
                SettingRow {
                    id: "cache_size",
                    label: "Cache Size Limit",
                    description: "Maximum disk space for caches. Older entries evicted first.",
                    control: SettingControl::Choice {
                        current: 0,
                        options: vec![
                            "50 MB".into(),
                            "100 MB".into(),
                            "250 MB".into(),
                            "500 MB".into(),
                            "1 GB".into(),
                            "Unlimited".into(),
                        ],
                    },
                },
                SettingRow {
                    id: "sep_debug",
                    label: "",
                    description: "",
                    control: SettingControl::Separator("━━━  Debug & Diagnostics  ━━━"),
                },
                SettingRow {
                    id: "export_config",
                    label: "Export Config to Clipboard",
                    description: "Copy current settings as TOML for backup or sharing.",
                    control: SettingControl::Action("Copy TOML"),
                },
                SettingRow {
                    id: "reset_defaults",
                    label: "Reset All to Defaults",
                    description: "Restore every setting to factory defaults (requires confirmation).",
                    control: SettingControl::Action("Reset"),
                },
            ],
        }
    }

    pub fn current_rows(&self) -> Vec<SettingRow> {
        self.rows_for_tab(self.tab)
    }

    pub fn next_tab(&mut self) {
        let idx = SettingsTab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        let next_idx = (idx + 1) % SettingsTab::ALL.len();
        self.tab = SettingsTab::ALL[next_idx];
        self.selected = 0;
        self.status_msg = None;
    }

    pub fn prev_tab(&mut self) {
        let idx = SettingsTab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        let prev_idx = if idx == 0 {
            SettingsTab::ALL.len() - 1
        } else {
            idx - 1
        };
        self.tab = SettingsTab::ALL[prev_idx];
        self.selected = 0;
        self.status_msg = None;
    }

    fn next_selectable(&self, from: usize, forward: bool) -> Option<usize> {
        let rows = self.current_rows();
        if rows.is_empty() {
            return None;
        }
        let len = rows.len();
        let mut idx = from;
        for _ in 0..len {
            idx = if forward {
                (idx + 1) % len
            } else if idx == 0 {
                len - 1
            } else {
                idx - 1
            };
            if !matches!(rows[idx].control, SettingControl::Separator(_)) {
                return Some(idx);
            }
        }
        None
    }

    pub fn next_row(&mut self) {
        if let Some(idx) = self.next_selectable(self.selected, true) {
            self.selected = idx;
        }
        self.status_msg = None;
    }

    pub fn prev_row(&mut self) {
        if let Some(idx) = self.next_selectable(self.selected, false) {
            self.selected = idx;
        }
        self.status_msg = None;
    }

    pub fn cycle_value(&mut self, forward: bool) -> Option<SettingsAction> {
        self.dirty = true;
        self.status_msg = None;
        let rows = self.current_rows();
        let row = rows.get(self.selected)?;
        if matches!(row.control, SettingControl::Separator(_)) {
            return None;
        }

        match row.id {
            "fps" => {
                let options = [30, 60, 120, 240, 1000];
                let current_idx = match self.animation_fps {
                    fps if fps <= 30 => 0,
                    fps if fps <= 60 => 1,
                    fps if fps <= 120 => 2,
                    fps if fps <= 240 => 3,
                    _ => 4,
                };
                let next_idx = if forward {
                    (current_idx + 1) % options.len()
                } else if current_idx == 0 {
                    options.len() - 1
                } else {
                    current_idx - 1
                };
                self.animation_fps = options[next_idx];
            }
            "theme" => {
                let options = [
                    "Adaptive",
                    "Tokyo Night",
                    "Catppuccin Mocha",
                    "Gruvbox Dark",
                    "Nord",
                    "Rosé Pine",
                    "Dracula",
                    "Monokai",
                    "Solarized Dark",
                ];
                let current_idx = match self.theme_name.to_lowercase().as_str() {
                    "tokyo night" | "tokyonight" => 1,
                    "catppuccin" | "catppuccin mocha" => 2,
                    "gruvbox" | "gruvbox dark" => 3,
                    "nord" => 4,
                    "rosé pine" | "rose pine" => 5,
                    "dracula" => 6,
                    "monokai" => 7,
                    "solarized" | "solarized dark" => 8,
                    _ => 0,
                };
                let next_idx = if forward {
                    (current_idx + 1) % options.len()
                } else if current_idx == 0 {
                    options.len() - 1
                } else {
                    current_idx - 1
                };
                self.theme_name = options[next_idx].to_string();
            }
            "theme_fade" => {
                if forward && self.theme_fade_speed < 3000 {
                    self.theme_fade_speed += 100;
                } else if !forward && self.theme_fade_speed > 200 {
                    self.theme_fade_speed -= 100;
                }
            }
            "viz_style" => {
                let all = VisualizerStyle::ALL;
                let current_idx = all.iter().position(|s| *s == self.visualizer_style).unwrap_or(0);
                let next_idx = if forward {
                    (current_idx + 1) % all.len()
                } else if current_idx == 0 {
                    all.len() - 1
                } else {
                    current_idx - 1
                };
                self.visualizer_style = all[next_idx];
            }
            "viz_smoothing" => {
                let all = VisualizerSmoothing::ALL;
                let current_idx = all.iter().position(|s| *s == self.visualizer_smoothing).unwrap_or(1);
                let next_idx = if forward {
                    (current_idx + 1) % all.len()
                } else if current_idx == 0 {
                    all.len() - 1
                } else {
                    current_idx - 1
                };
                self.visualizer_smoothing = all[next_idx];
            }
            "viz_bar_width" => {
                if forward && self.visualizer_bar_width < 4 {
                    self.visualizer_bar_width += 1;
                } else if !forward && self.visualizer_bar_width > 1 {
                    self.visualizer_bar_width -= 1;
                }
            }
            "viz_colors" => {
                let max = 5;
                if forward && self.visualizer_color_scheme < max {
                    self.visualizer_color_scheme += 1;
                } else if !forward && self.visualizer_color_scheme > 0 {
                    self.visualizer_color_scheme -= 1;
                }
            }
            "progress_style" => {
                let max = 4;
                if forward && self.progress_bar_style < max {
                    self.progress_bar_style += 1;
                } else if !forward && self.progress_bar_style > 0 {
                    self.progress_bar_style -= 1;
                }
            }
            "protocol" => {
                let options = [None, Some("kitty"), Some("sixel"), Some("iterm2"), Some("halfblocks")];
                let current_idx = match self.protocol.as_deref() {
                    Some("kitty") => 1,
                    Some("sixel") => 2,
                    Some("iterm2") => 3,
                    Some("halfblocks") => 4,
                    _ => 0,
                };
                let next_idx = if forward {
                    (current_idx + 1) % options.len()
                } else if current_idx == 0 {
                    options.len() - 1
                } else {
                    current_idx - 1
                };
                self.protocol = options[next_idx].map(String::from);
            }
            "zen" => self.zen_default = !self.zen_default,

            // Playback
            "audio_quality" => {
                let all = AudioQuality::ALL;
                let current_idx = all.iter().position(|q| *q == self.audio_quality).unwrap_or(0);
                let next_idx = if forward {
                    (current_idx + 1) % all.len()
                } else if current_idx == 0 {
                    all.len() - 1
                } else {
                    current_idx - 1
                };
                self.audio_quality = all[next_idx];
            }
            "buffer_duration" => {
                if forward && self.buffer_duration_secs < 15 {
                    self.buffer_duration_secs += 1;
                } else if !forward && self.buffer_duration_secs > 1 {
                    self.buffer_duration_secs -= 1;
                }
            }
            "crossfade" => self.crossfade_enabled = !self.crossfade_enabled,
            "crossfade_duration" => {
                if forward && self.crossfade_duration_ms < 10000 {
                    self.crossfade_duration_ms += 250;
                } else if !forward && self.crossfade_duration_ms > 500 {
                    self.crossfade_duration_ms -= 250;
                }
            }
            "gapless" => self.gapless_playback = !self.gapless_playback,
            "replay_gain" => self.replay_gain = !self.replay_gain,
            "volume_step" => {
                if forward && self.volume_step < 25 {
                    self.volume_step += 1;
                } else if !forward && self.volume_step > 1 {
                    self.volume_step -= 1;
                }
            }
            "restore_on_startup" => self.restore_on_startup = !self.restore_on_startup,
            "next_track_prefetch" => self.next_track_prefetch = !self.next_track_prefetch,

            // Lyrics
            "lyrics_align" => {
                let all = LyricsAlignment::ALL;
                let current_idx = all.iter().position(|a| *a == self.lyrics_alignment).unwrap_or(0);
                let next_idx = if forward {
                    (current_idx + 1) % all.len()
                } else if current_idx == 0 {
                    all.len() - 1
                } else {
                    current_idx - 1
                };
                self.lyrics_alignment = all[next_idx];
            }
            "lyrics_font_size" => {
                if forward && self.lyrics_font_size < 150 {
                    self.lyrics_font_size += 10;
                } else if !forward && self.lyrics_font_size > 80 {
                    self.lyrics_font_size -= 10;
                }
            }
            "lyrics_highlight" => {
                let max = 4;
                if forward && self.lyrics_highlight_color < max {
                    self.lyrics_highlight_color += 1;
                } else if !forward && self.lyrics_highlight_color > 0 {
                    self.lyrics_highlight_color -= 1;
                }
            }
            "lyrics_auto_scroll" => self.lyrics_auto_scroll = !self.lyrics_auto_scroll,
            "lyrics_transliterate" => self.lyrics_transliterate = !self.lyrics_transliterate,

            // Interface
            "search_limit" => {
                if forward && self.search_limit < 50 {
                    self.search_limit += 5;
                } else if !forward && self.search_limit > 5 {
                    self.search_limit -= 5;
                }
            }
            "scrolloff" => {
                if forward && self.scrolloff < 15 {
                    self.scrolloff += 1;
                } else if !forward && self.scrolloff > 0 {
                    self.scrolloff -= 1;
                }
            }
            "ui_density" => {
                let max = 2;
                if forward && self.ui_density < max {
                    self.ui_density += 1;
                } else if !forward && self.ui_density > 0 {
                    self.ui_density -= 1;
                }
            }
            "show_art_queue" => self.show_album_art_in_queue = !self.show_album_art_in_queue,
            "show_progress_footer" => self.show_progress_in_footer = !self.show_progress_in_footer,
            "footer_clock" => {
                let max = 3;
                if forward && self.footer_clock_format < max {
                    self.footer_clock_format += 1;
                } else if !forward && self.footer_clock_format > 0 {
                    self.footer_clock_format -= 1;
                }
            }
            "sidebar_width" => {
                if forward && self.sidebar_width_pct < 50 {
                    self.sidebar_width_pct += 5;
                } else if !forward && self.sidebar_width_pct > 20 {
                    self.sidebar_width_pct -= 5;
                }
            }

            // System
            "mpris" => self.mpris_enabled = !self.mpris_enabled,
            "clear_cache" => return Some(SettingsAction::ClearCache),
            "cache_size" => {
                let max = 5;
                if forward && self.visualizer_color_scheme < max {
                    // reuse field for cache size selection
                } else if !forward && self.visualizer_color_scheme > 0 {
                }
            }
            "export_config" => {
                self.status_msg = Some("Config exported to clipboard (not yet implemented)".to_string());
            }
            "reset_defaults" => {
                self.status_msg = Some("Reset requires confirmation (not yet implemented)".to_string());
            }
            _ => {}
        }
        None
    }

    pub fn apply_to_config(&self, c: &mut Config) {
        // Visuals
        c.animation_fps = self.animation_fps;
        c.visualizer_style = self.visualizer_style;
        c.visualizer_smoothing = self.visualizer_smoothing;
        c.visualizer_bar_width = self.visualizer_bar_width;
        c.visualizer_color_scheme = self.visualizer_color_scheme;
        c.progress_bar_style = self.progress_bar_style;
        c.protocol = self.protocol.clone();
        c.zen_default = self.zen_default;
        c.theme_name = self.theme_name.clone();
        c.theme_fade_speed = self.theme_fade_speed;

        // Playback
        c.audio_quality = self.audio_quality;
        c.buffer_duration_secs = self.buffer_duration_secs;
        c.volume_step = self.volume_step;
        c.crossfade_enabled = self.crossfade_enabled;
        c.crossfade_duration_ms = self.crossfade_duration_ms;
        c.gapless_playback = self.gapless_playback;
        c.restore_on_startup = self.restore_on_startup;
        c.next_track_prefetch = self.next_track_prefetch;
        c.replay_gain = self.replay_gain;

        // Lyrics
        c.lyrics_alignment = self.lyrics_alignment;
        c.lyrics_transliterate = self.lyrics_transliterate;
        c.lyrics_auto_scroll = self.lyrics_auto_scroll;
        c.lyrics_font_size = self.lyrics_font_size;
        c.lyrics_highlight_color = self.lyrics_highlight_color;

        // Interface
        c.search_limit = self.search_limit;
        c.scrolloff = self.scrolloff;
        c.ui_density = self.ui_density;
        c.show_album_art_in_queue = self.show_album_art_in_queue;
        c.show_progress_in_footer = self.show_progress_in_footer;
        c.footer_clock_format = self.footer_clock_format;
        c.sidebar_width_pct = self.sidebar_width_pct;

        // System
        c.mpris_enabled = self.mpris_enabled;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_state_initializes_and_cycles_tabs() {
        let config = Config::default();
        let mut state = SettingsState::init_from_config(&config);
        assert_eq!(state.tab, SettingsTab::Visuals);

        state.next_tab();
        assert_eq!(state.tab, SettingsTab::Playback);
        state.next_tab();
        assert_eq!(state.tab, SettingsTab::Lyrics);
        state.next_tab();
        assert_eq!(state.tab, SettingsTab::Interface);
        state.next_tab();
        assert_eq!(state.tab, SettingsTab::System);
        state.next_tab();
        assert_eq!(state.tab, SettingsTab::Visuals);

        // Navigation between rows
        let rows = state.current_rows();
        assert!(!rows.is_empty());
        state.next_row();
        assert_eq!(state.selected, 1);
        state.prev_row();
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn settings_state_cycles_values_and_applies() {
        let mut config = Config::default();
        let mut state = SettingsState::init_from_config(&config);

        // Row 0 is FPS (30 -> 60 -> 120 -> 240 -> 1000)
        state.selected = 0;
        state.cycle_value(true);
        assert_eq!(state.animation_fps, 240);

        // Row 1 is Theme
        state.selected = 1;
        state.cycle_value(true);
        assert_eq!(state.theme_name, "Tokyo Night");

        state.apply_to_config(&mut config);
        assert_eq!(config.animation_fps, 240);
        assert_eq!(config.theme_name, "Tokyo Night");
    }
}