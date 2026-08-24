<div align="center">

# 🐟 Tuna TUI

**A lean, beautiful terminal music player for YouTube & YouTube Music.**

[![CI](https://github.com/shrijit37/tuna-tui/actions/workflows/ci.yml/badge.svg)](https://github.com/shrijit37/tuna-tui/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tuna-tui.svg)](https://crates.io/crates/tuna-tui)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)

*Fast, minimal, and fully featured. Powered by Rust + [Ratatui], playing YouTube streams via `yt-dlp → ffmpeg → rodio` with live FFT audio visualizer, synchronized lyrics, cover art color palettes, and MPRIS media keys.*

</div>

---

## ✨ Features

- 🔍 **Instant Search** — Fast YouTube & YouTube Music InnerTube search with query suggestions.
- 📻 **Endless Radio** — Smart radio track recommendations from any seed song or artist.
- 📜 **Synchronized Lyrics** — Live scrolling lyrics from `lrclib.net` with InnerTube fallback and automatic Indic/Devanagari transliteration.
- 🎨 **Adaptive Cover Art & Themes** — High-resolution album artwork (Sixel/Kitty graphics) with real-time dynamic palette generation.
- 📊 **Audio Spectrum Visualizer** — Real-time FFT visualizer (60–120 FPS) with customizable decay and visual styles.
- ⚙️ **Interactive Settings Menu** — Dual-pane configuration menu with 5 tabs and instant TOML persistence.
- 💾 **Local Library & History** — Liked songs, custom playlists with inline creation, and history tracking.
- 🔄 **Resilient Playback Engine** — Automatic recovery from transient stream drops and mid-track reconnects.
- 🎹 **Media Keys & MPRIS** — Full hardware media key and system desktop integration via Souvlaki/zbus.
- 🛠️ **Standalone TXC Protocol** — Decoupled terminal color and theme protocol available as a standalone library/CLI.

---

## 📋 Requirements

- **`yt-dlp`** and **`ffmpeg`** installed and available on your `PATH`.
- A terminal with truecolor support (e.g. Foot, Kitty, WezTerm, Alacritty, Ghostty, iTerm2).

---

## 📦 Installation

### Cargo (crates.io)
```bash
cargo install tuna-tui
```

### Arch Linux (AUR)
```bash
paru -S tuna-tui
# or
yay -S tuna-tui
```

### Homebrew (macOS / Linux)
```bash
brew install shrijit37/homebrew-tap/tuna-tui
```

### Nix / NixOS
```bash
nix run github:shrijit37/tuna-tui
```

### From Source
```bash
git clone https://github.com/shrijit37/tuna-tui.git
cd tuna-tui
cargo build --release
# Binary available at target/release/tuna-tui
```

---

## ⌨️ Keybindings

| Key | Action |
|---|---|
| `j` / `k`, `↑` / `↓` | Move selection down / up |
| `Enter` | Open / drill into playlist, album, artist; run search |
| `Shift+Enter` | Play highlighted item directly (CSI-u terminals) |
| `Tab` / `]`, `Shift+Tab` / `[` | Next / previous library section |
| `←` / `→` | Switch right-pane view (Now Playing / Queue / Lyrics) |
| `Shift+←` / `Shift+→` | Seek −5s / +5s |
| `/` | Open search prompt (`Esc` to cancel, `Ctrl+U` to clear) |
| `Space` / `p` | Play / pause |
| `n` / `b` | Next / previous track |
| `+` / `=` , `-` / `_` | Volume up / down |
| `s` / `S` | Toggle shuffle / shuffle & play current list |
| `R` / `r` | Toggle repeat mode / refresh library |
| `a` | Open actions menu (Like, Add to Queue, Copy Link, etc.) |
| `o` | Cycle sorting mode for active list |
| `z` | Toggle Zen Mode (hide sidebar) |
| `q` | Quit application |

---

## ⚙️ Configuration

Configuration is automatically persisted to `~/.config/tuna-tui/config.toml`. You can configure settings directly through the in-app **Settings Menu** or by editing the TOML file:

```toml
search_limit = 20
volume = 80
animation_fps = 120
volume_step = 5
next_track_prefetch = true

[visualizer]
style = "bars"
smoothing = true

[lyrics]
alignment = "center"
transliterate_indic = true
```

---

## 🤝 Contributing

Contributions are warmly welcomed! Please check out [CONTRIBUTING.md](CONTRIBUTING.md) for details on setting up the development environment, running tests, and submitting PRs.

Please also review our [Code of Conduct](CODE_OF_CONDUCT.md).

---

## 📜 Changelog

See [CHANGELOG.md](CHANGELOG.md) for a detailed history of changes and release notes.

---

## 🛡️ Security

Please report any security concerns following our [Security Policy](SECURITY.md).

---

## 📄 License

Tuna TUI is licensed under the [MIT License](LICENSE).

[Ratatui]: https://ratatui.rs
