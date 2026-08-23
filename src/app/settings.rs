//! Settings state model, tabs, and interactive option descriptors.

use tuna_tui::config::{AudioQuality, Config, LyricsAlignment, VisualizerSmoothing, VisualizerStyle};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsTab {
    #[default]
    Display,
    Audio,
    Lyrics,
    Search,
    System,
}

impl SettingsTab {
    pub const ALL: [SettingsTab; 5] = [
        SettingsTab::Display,
        SettingsTab::Audio,
        SettingsTab::Lyrics,
        SettingsTab::Search,
        SettingsTab::System,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Display => "Display & Motion",
            Self::Audio => "Audio & Playback",
            Self::Lyrics => "Lyrics",
            Self::Search => "Search & Library",
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

    // Live mutable settings values
    pub animation_fps: u16,
    pub visualizer_style: VisualizerStyle,
    pub visualizer_smoothing: VisualizerSmoothing,
    pub protocol: Option<String>,
    pub zen_default: bool,
    pub theme_name: String,

    pub audio_quality: AudioQuality,
    pub buffer_duration_secs: u8,
    pub volume_step: u8,
    pub restore_on_startup: bool,
    pub next_track_prefetch: bool,

    pub lyrics_alignment: LyricsAlignment,
    pub lyrics_transliterate: bool,

    pub search_limit: usize,
    pub scrolloff: usize,

    pub mpris_enabled: bool,
    pub status_msg: Option<String>,
}

impl SettingsState {
    pub fn init_from_config(c: &Config) -> Self {
        Self {
            tab: SettingsTab::Display,
            selected: 0,
            dirty: false,

            animation_fps: c.animation_fps,
            visualizer_style: c.visualizer_style,
            visualizer_smoothing: c.visualizer_smoothing,
            protocol: c.protocol.clone(),
            zen_default: c.zen_default,
            theme_name: c.theme_name.clone(),

            audio_quality: c.audio_quality,
            buffer_duration_secs: c.buffer_duration_secs,
            volume_step: c.volume_step,
            restore_on_startup: c.restore_on_startup,
            next_track_prefetch: c.next_track_prefetch,

            lyrics_alignment: c.lyrics_alignment,
            lyrics_transliterate: c.lyrics_transliterate,

            search_limit: c.search_limit,
            scrolloff: c.scrolloff,

            mpris_enabled: c.mpris_enabled,
            status_msg: None,
        }
    }

    pub fn rows_for_tab(&self, tab: SettingsTab) -> Vec<SettingRow> {
        match tab {
            SettingsTab::Display => vec![
                SettingRow {
                    id: "fps",
                    label: "Motion & Animation FPS",
                    description: "Target render frame rate for visualizer and lyrics smooth scrolling.",
                    control: SettingControl::Choice {
                        current: match self.animation_fps {
                            fps if fps <= 30 => 0,
                            fps if fps <= 60 => 1,
                            fps if fps <= 120 => 2,
                            fps if fps <= 240 => 3,
                            _ => 4,
                        },
                        options: vec![
                            "30 FPS (33ms)".into(),
                            "60 FPS (16ms)".into(),
                            "120 FPS (8ms)".into(),
                            "240 FPS (4ms)".into(),
                            "1ms (Uncapped)".into(),
                        ],
                    },
                },
                SettingRow {
                    id: "viz_style",
                    label: "Visualizer Style",
                    description: "Character rendering glyph style for the spectrum visualizer.",
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
                    label: "Visualizer Smoothing",
                    description: "Frequency band spatial smoothing and peak decay envelope.",
                    control: SettingControl::Choice {
                        current: VisualizerSmoothing::ALL
                            .iter()
                            .position(|s| *s == self.visualizer_smoothing)
                            .unwrap_or(1),
                        options: VisualizerSmoothing::ALL.iter().map(|s| s.label().to_string()).collect(),
                    },
                },
                SettingRow {
                    id: "protocol",
                    label: "Album Art Protocol",
                    description: "Terminal graphics protocol for album art (Kitty, Sixel, iTerm2, Halfblocks).",
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
                            "Kitty".into(),
                            "Sixel".into(),
                            "iTerm2".into(),
                            "Halfblocks".into(),
                        ],
                    },
                },
                SettingRow {
                    id: "zen",
                    label: "Default to Zen Mode",
                    description: "Launch Tuna TUI in fullscreen Now Playing mode without sidebar.",
                    control: SettingControl::Toggle(self.zen_default),
                },
                SettingRow {
                    id: "theme",
                    label: "UI Theme Palette",
                    description: "Color palette and styling for all UI components.",
                    control: SettingControl::Choice {
                        current: match self.theme_name.to_lowercase().as_str() {
                            "tokyo night" | "tokyonight" => 1,
                            "catppuccin" | "catppuccin mocha" => 2,
                            "gruvbox" => 3,
                            "nord" => 4,
                            "rosé pine" | "rose pine" => 5,
                            "dracula" => 6,
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
                        ],
                    },
                },
            ],
            SettingsTab::Audio => vec![
                SettingRow {
                    id: "audio_quality",
                    label: "Audio Stream Quality",
                    description: "YouTube audio stream format resolution preference.",
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
                    label: "Stream Prebuffer Duration",
                    description: "Seconds of audio buffered before playback starts (prevents stutter).",
                    control: SettingControl::Number {
                        val: self.buffer_duration_secs as i64,
                        min: 1,
                        max: 10,
                        step: 1,
                        suffix: " s",
                    },
                },
                SettingRow {
                    id: "volume_step",
                    label: "Volume Increment Step",
                    description: "Percentage volume changes by per +/- keypress.",
                    control: SettingControl::Number {
                        val: self.volume_step as i64,
                        min: 1,
                        max: 20,
                        step: 1,
                        suffix: " %",
                    },
                },
                SettingRow {
                    id: "restore_on_startup",
                    label: "Resume on Startup",
                    description: "Resume last played track and queue position on app launch.",
                    control: SettingControl::Toggle(self.restore_on_startup),
                },
                SettingRow {
                    id: "next_track_prefetch",
                    label: "Next-Track Audio Prefetching",
                    description: "Resolve next song audio stream in background for seamless transitions.",
                    control: SettingControl::Toggle(self.next_track_prefetch),
                },
            ],
            SettingsTab::Lyrics => vec![
                SettingRow {
                    id: "lyrics_align",
                    label: "Lyrics Alignment",
                    description: "Horizontal text alignment for verses in Lyrics view.",
                    control: SettingControl::Choice {
                        current: LyricsAlignment::ALL
                            .iter()
                            .position(|a| *a == self.lyrics_alignment)
                            .unwrap_or(0),
                        options: LyricsAlignment::ALL.iter().map(|a| a.label().to_string()).collect(),
                    },
                },
                SettingRow {
                    id: "lyrics_transliterate",
                    label: "Auto-Transliteration",
                    description: "Transliterate non-Latin (Indic/CJK) lyrics to Latin alphabet phonetics.",
                    control: SettingControl::Toggle(self.lyrics_transliterate),
                },
            ],
            SettingsTab::Search => vec![
                SettingRow {
                    id: "search_limit",
                    label: "Search Result Limit",
                    description: "Maximum number of search result tracks returned per query.",
                    control: SettingControl::Number {
                        val: self.search_limit as i64,
                        min: 3,
                        max: 25,
                        step: 1,
                        suffix: " tracks",
                    },
                },
                SettingRow {
                    id: "scrolloff",
                    label: "List Cursor Scrolloff",
                    description: "Number of screen rows kept visible above and below selection cursor.",
                    control: SettingControl::Number {
                        val: self.scrolloff as i64,
                        min: 0,
                        max: 10,
                        step: 1,
                        suffix: " rows",
                    },
                },
            ],
            SettingsTab::System => vec![
                SettingRow {
                    id: "mpris",
                    label: "System Media Keys / MPRIS",
                    description: "Enable OS media controls, keyboard media keys, and playerctl support.",
                    control: SettingControl::Toggle(self.mpris_enabled),
                },
                SettingRow {
                    id: "clear_cache",
                    label: "Clear Local Cache",
                    description: "Purge cached lyrics and album artwork from ~/.cache/tuna-tui/.",
                    control: SettingControl::Action("Clear Now"),
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

    pub fn next_row(&mut self) {
        let len = self.current_rows().len();
        if len > 0 {
            self.selected = (self.selected + 1) % len;
        }
        self.status_msg = None;
    }

    pub fn prev_row(&mut self) {
        let len = self.current_rows().len();
        if len > 0 {
            self.selected = if self.selected == 0 {
                len - 1
            } else {
                self.selected - 1
            };
        }
        self.status_msg = None;
    }

    pub fn cycle_value(&mut self, forward: bool) -> Option<SettingsAction> {
        self.dirty = true;
        self.status_msg = None;
        let rows = self.current_rows();
        let row = rows.get(self.selected)?;

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
            "theme" => {
                let options = ["Adaptive", "Tokyo Night", "Catppuccin Mocha", "Gruvbox Dark", "Nord", "Rosé Pine", "Dracula"];
                let current_idx = match self.theme_name.to_lowercase().as_str() {
                    "tokyo night" | "tokyonight" => 1,
                    "catppuccin" | "catppuccin mocha" => 2,
                    "gruvbox" | "gruvbox dark" => 3,
                    "nord" => 4,
                    "rosé pine" | "rose pine" => 5,
                    "dracula" => 6,
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
                if forward && self.buffer_duration_secs < 10 {
                    self.buffer_duration_secs += 1;
                } else if !forward && self.buffer_duration_secs > 1 {
                    self.buffer_duration_secs -= 1;
                }
            }
            "volume_step" => {
                if forward && self.volume_step < 20 {
                    self.volume_step += 1;
                } else if !forward && self.volume_step > 1 {
                    self.volume_step -= 1;
                }
            }
            "restore_on_startup" => self.restore_on_startup = !self.restore_on_startup,
            "next_track_prefetch" => self.next_track_prefetch = !self.next_track_prefetch,
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
            "lyrics_transliterate" => self.lyrics_transliterate = !self.lyrics_transliterate,
            "search_limit" => {
                if forward && self.search_limit < 25 {
                    self.search_limit += 1;
                } else if !forward && self.search_limit > 3 {
                    self.search_limit -= 1;
                }
            }
            "scrolloff" => {
                if forward && self.scrolloff < 10 {
                    self.scrolloff += 1;
                } else if !forward && self.scrolloff > 0 {
                    self.scrolloff -= 1;
                }
            }
            "mpris" => self.mpris_enabled = !self.mpris_enabled,
            "clear_cache" => return Some(SettingsAction::ClearCache),
            _ => {}
        }
        None
    }

    pub fn apply_to_config(&self, c: &mut Config) {
        c.animation_fps = self.animation_fps;
        c.visualizer_style = self.visualizer_style;
        c.visualizer_smoothing = self.visualizer_smoothing;
        c.protocol = self.protocol.clone();
        c.zen_default = self.zen_default;
        c.theme_name = self.theme_name.clone();

        c.audio_quality = self.audio_quality;
        c.buffer_duration_secs = self.buffer_duration_secs;
        c.volume_step = self.volume_step;
        c.restore_on_startup = self.restore_on_startup;
        c.next_track_prefetch = self.next_track_prefetch;

        c.lyrics_alignment = self.lyrics_alignment;
        c.lyrics_transliterate = self.lyrics_transliterate;

        c.search_limit = self.search_limit;
        c.scrolloff = self.scrolloff;

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
        assert_eq!(state.tab, SettingsTab::Display);
        assert_eq!(state.selected, 0);

        state.next_tab();
        assert_eq!(state.tab, SettingsTab::Audio);
        assert_eq!(state.selected, 0);

        state.prev_tab();
        assert_eq!(state.tab, SettingsTab::Display);
    }

    #[test]
    fn settings_state_cycles_values_and_applies() {
        let mut config = Config::default();
        let mut state = SettingsState::init_from_config(&config);

        // Row 0 is FPS (30 -> 60 -> 120 -> 240 -> 1000)
        state.selected = 0;
        state.cycle_value(true);
        assert_eq!(state.animation_fps, 240);

        // Row 4 is Zen mode toggle
        state.selected = 4;
        state.cycle_value(true);
        assert!(state.zen_default);

        state.apply_to_config(&mut config);
        assert_eq!(config.animation_fps, 240);
        assert!(config.zen_default);
    }
}
