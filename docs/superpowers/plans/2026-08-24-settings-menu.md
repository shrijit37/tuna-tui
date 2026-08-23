# Settings Menu Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a comprehensive, interactive Settings Menu overlay to `tuna-tui` allowing users to configure Display & Motion (FPS, Graphics Protocol, Visualizer Style), Audio & Playback (Quality, Buffer, Volume Step), Theme & Appearance, Lyrics Preferences, Search Settings, and System/Cache management, with live updates and automatic persistence to `~/.config/tuna-tui/config.toml`.

**Architecture:** 
- Extend `src/config.rs` with new settings fields, enums, parser defaults, and an atomic TOML serializer/writer.
- Introduce `SettingsState` in `src/app/state.rs` modeling categorized settings items (Toggles, Selectors, Action buttons, Numeric steppers) and keyboard navigation.
- Add `src/ui/settings.rs` rendering a dual-pane modal (Categories on left, interactive options on right) with custom widget styling.
- Wire input event handling in `src/input/key.rs` (`Ctrl+,`, `S`, or Action menu `⚙ Settings`) with live state mutation and `Esc` to close & persist.

**Tech Stack:** Rust, Ratatui, TOML serialization, Tokio async channels.

**Spec:** Settings specification from UX/UI audit (Categories: Display, Audio, Theme, Lyrics, Search, System).

## Global Constraints
- Every setting must have a safe fallback default if absent or corrupted in `config.toml`.
- Changing a setting in the UI must immediately apply to live runtime state (e.g. changing FPS or theme must take effect without requiring an app restart).
- Closing settings (`Esc`) must atomically persist changes to `~/.config/tuna-tui/config.toml` without blocking the render loop.
- All code must pass `cargo fmt`, `cargo clippy -D warnings`, and all unit/integration test suites.

---

### Task 1: Extend `Config` Struct and TOML Persistence

**Files:**
- Modify: `src/config.rs`
- Test: `src/config.rs:mod tests`

**Interfaces:**
- Produces:
  - `pub enum VisualizerStyle { Block, Braille, Line, Solid }`
  - `pub enum AudioQuality { High, Best, DataSaver }`
  - `pub enum LyricsAlignment { Center, Left, Right }`
  - `pub struct Config { pub animation_fps: u16, pub visualizer_style: VisualizerStyle, pub volume_step: u8, pub lyrics_alignment: LyricsAlignment, pub lyrics_transliterate: bool, pub next_track_prefetch: bool, ... }`
  - `pub fn Config::save_to_file(&self, path: &Path) -> std::io::Result<()>`

- [ ] **Step 1: Write failing tests for new config fields and serialization**

```rust
#[test]
fn config_parses_and_serializes_extended_settings() {
    let toml = r#"
animation_fps = 120
volume_step = 5
visualizer_style = "braille"
audio_quality = "high"
lyrics_alignment = "center"
lyrics_transliterate = true
next_track_prefetch = true
"#;
    let config = Config::parse(toml);
    assert_eq!(config.animation_fps, 120);
    assert_eq!(config.volume_step, 5);
    assert_eq!(config.visualizer_style, VisualizerStyle::Braille);
    assert_eq!(config.audio_quality, AudioQuality::High);
    assert_eq!(config.lyrics_alignment, LyricsAlignment::Center);
    assert!(config.lyrics_transliterate);
    assert!(config.next_track_prefetch);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test config_parses_and_serializes_extended_settings`
Expected: FAIL (missing fields/types)

- [ ] **Step 3: Implement new enums, struct fields, and TOML serialization in `src/config.rs`**

Add enums with `serde::Serialize`, `serde::Deserialize`, implement `Config::save_to_file` and parse helpers.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --all-targets`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git commit -am "feat(config): add extended settings fields and TOML persistence"
```

---

### Task 2: Implement Settings State Model & Item Hierarchy

**Files:**
- Create: `src/app/settings.rs`
- Modify: `src/app/mod.rs`, `src/app/state.rs`
- Test: `src/main_tests/settings.rs`

**Interfaces:**
- Produces:
  - `pub enum SettingsTab { Display, Audio, Theme, Lyrics, Search, System }`
  - `pub enum SettingValue { Toggle(bool), Choice { current: usize, options: Vec<&'static str> }, Action(&'static str), Number { val: i64, min: i64, max: i64, step: i64, suffix: &'static str } }`
  - `pub struct SettingRow { pub id: &'static str, pub label: &'static str, pub description: &'static str, pub value: SettingValue }`
  - `pub struct SettingsState { pub tab: SettingsTab, pub selected: usize, pub dirty: bool, pub items: Vec<SettingRow> }`
  - `impl SettingsState { pub fn next_row(&mut self); pub fn prev_row(&mut self); pub fn next_tab(&mut self); pub fn prev_tab(&mut self); pub fn next_value(&mut self, app: &mut App); pub fn prev_value(&mut self, app: &mut App); pub fn toggle_or_act(&mut self, app: &mut App); }`

- [ ] **Step 1: Write failing tests for `SettingsState` navigation and value cycling**

```rust
#[test]
fn settings_state_cycles_tabs_and_mutates_values() {
    let mut state = SettingsState::init_from_config(Config::get());
    assert_eq!(state.tab, SettingsTab::Display);
    state.next_tab();
    assert_eq!(state.tab, SettingsTab::Audio);
    state.prev_tab();
    assert_eq!(state.tab, SettingsTab::Display);
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test settings_state_cycles_tabs_and_mutates_values`
Expected: FAIL

- [ ] **Step 3: Implement `src/app/settings.rs` and attach `settings: Option<SettingsState>` to `AppState`**

Implement initialization from live config, tab filtering, row navigation, and value mutation handlers.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --all-targets`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git commit -am "feat(app): implement settings state model and value mutation handlers"
```

---

### Task 3: Settings UI Modal & Widget Renderer

**Files:**
- Create: `src/ui/settings.rs`
- Modify: `src/ui/mod.rs`
- Test: `src/main_tests/ui.rs`

**Interfaces:**
- Produces:
  - `pub(crate) fn render_settings_overlay(f: &mut Frame, app: &App, theme: Theme, area: Rect)`

- [ ] **Step 1: Write UI snapshot test for settings rendering**

```rust
#[test]
fn settings_overlay_renders_categories_and_controls() {
    let mut app = App::mock();
    app.view.settings = Some(SettingsState::init_from_config(Config::get()));
    let out = render_to_test_buffer(&app);
    assert!(out.contains("⚙  Settings"));
    assert!(out.contains("Display"));
    assert!(out.contains("Audio"));
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test settings_overlay_renders_categories_and_controls`
Expected: FAIL

- [ ] **Step 3: Implement `src/ui/settings.rs` with dual-pane layout**

- Render category sidebar with active category highlighting and counts.
- Render right pane rows with custom controls:
  - `[✓] Enabled` / `[ ] Disabled` for toggles
  - `◀ 120 FPS ▶` for choice steppers
  - `[ Clear Cache ]` for buttons
  - Subtle help/description text beneath each selected item.
- Wire `render_settings_overlay` into `src/ui/mod.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --all-targets`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git commit -am "feat(ui): implement dual-pane settings overlay renderer"
```

---

### Task 4: Input Handling, Keybindings & Live Application

**Files:**
- Modify: `src/input/key.rs`, `src/actions.rs`, `src/app/event.rs`
- Test: `src/main_tests/nav.rs`

**Interfaces:**
- Consumes: `SettingsState`, `render_settings_overlay`, `Config::save_to_file`
- Produces:
  - Key bindings when `app.view.settings.is_some()`:
    - `Esc` / `q`: Close settings and persist changes
    - `Tab` / `Shift-Tab` / `h` / `l` (on category): Cycle categories
    - `Up` / `Down` / `k` / `j`: Move item selection
    - `Left` / `Right` / `h` / `l`: Adjust value / cycle options
    - `Enter` / `Space`: Toggle boolean or execute action
  - Action item in `src/actions.rs`: `"⚙  Settings"`

- [ ] **Step 1: Write integration tests for settings key interactions**

```rust
#[test]
fn pressing_esc_saves_and_closes_settings() {
    let mut app = App::mock();
    app.open_settings();
    assert!(app.view.settings.is_some());
    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, &chans);
    assert!(app.view.settings.is_none());
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test pressing_esc_saves_and_closes_settings`
Expected: FAIL

- [ ] **Step 3: Implement keyboard routing in `src/input/key.rs` and live config updates**

- Intercept keys when `app.view.settings.is_some()`.
- Add `"⚙  Settings"` to Action menu.
- On close, spawn async task or background write to update `~/.config/tuna-tui/config.toml`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --all-targets`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git commit -am "feat(input): add keyboard navigation and live persistence for settings menu"
```

---

### Task 5: Full Integration Verification & Quality Gates

**Files:**
- Run: `cargo fmt --check`, `RUSTFLAGS="-D warnings" cargo clippy --all-targets`, `cargo test --all-targets`

- [ ] **Step 1: Run full lint and test suites**
- [ ] **Step 2: Verify zero clippy warnings and 100% test pass rate**
- [ ] **Step 3: Merge into `monday` and `master`**

---
