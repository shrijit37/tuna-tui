# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.4.0] - 2026-08-23

### Added
- **YouTube Music InnerTube Integration**: Native InnerTube API client for fast search, track suggestions, and square album artwork.
- **Interactive Dual-Pane Settings Menu**: Customization menu with 5 category tabs (Visuals, Playback, Lyrics, Interface, System) and live runtime toggles.
- **Debug Mode Diagnostics**: Real-time diagnostic overlays for network, audio sink, FFT visualizer, cache, and playback recovery monitoring.
- **Lyrics Disk Caching & Prefetching**: Fast on-disk LRU cache for synchronized lyrics (`lrclib.net` + InnerTube fallback) and background prefetching for upcoming queue tracks.
- **Indic Script Transliteration**: Automatic romanization/transliteration for Devanagari/Indic scripts to maintain terminal layout fidelity.
- **Inline Playlist Creation**: Always-on-top playlist creator input in the library sidebar.
- **Enhanced Spectrum Visualizer**: 60–120fps FFT visualizer with continuous decay curves and multiple visualization modes.
- **Atomic State Persistence**: Safe atomic JSON state persistence for history, library, and volume mixer state.
- **Packaging & Distribution**: Automated release workflows for Homebrew formula, Arch Linux AUR (`PKGBUILD`), and Nix Flake.

### Changed
- **Pure Streaming Engine**: Clean `yt-dlp → ffmpeg → rodio` audio pipeline with zero Spotify/OAuth dependencies.
- **Stream Recovery**: Mid-track auto-reconnect and transient stream drop detection.
- **Queue Rendering**: Queue list now resolves and renders human-readable title and artist labels instead of raw stream URIs.

### Fixed
- Fixed progress bar cell offset and right-aligned duration timestamp rendering.
- Fixed cover art center-cropping to maintain 1:1 aspect ratio on high-DPI terminals.
- Fixed playlist track count updates in the left library panel.
- Fixed artist and album drill-in track selection.

---

## [0.3.0] - 2026-08-16

### Added
- Core TUI shell built on Ratatui and Crossterm.
- Initial yt-dlp search and streaming harness.
- Basic rodio audio sink and volume mixer.
- Local library storage for liked tracks and playlists.
- TXC terminal color protocol implementation.

---

## [0.1.0] - 2026-07-15

### Added
- Initial project scaffold and Cargo manifest.
