# Contributing to Tuna TUI

Thank you for your interest in contributing to Tuna TUI! We welcome contributions of all kinds: bug reports, documentation improvements, feature proposals, and code contributions.

Please review this guide before submitting a pull request or opening an issue.

---

## Code of Conduct

All contributors and maintainers are expected to adhere to our [Code of Conduct](CODE_OF_CONDUCT.md). Please treat everyone with respect and kindness.

---

## Getting Started

### Prerequisites

Tuna TUI requires:
- **Rust Toolchain**: Stable Rust (1.75 or later) via [rustup](https://rustup.rs).
- **Runtime Dependencies**:
  - `yt-dlp` (on `PATH`)
  - `ffmpeg` (on `PATH`)
- **System Libraries (Linux)**:
  - `libasound2-dev` (ALSA audio headers)
  - `libssl-dev` (OpenSSL headers)
  - `pkg-config`

#### Installing Prerequisites

**Ubuntu / Debian**:
```bash
sudo apt-get update
sudo apt-get install -y ffmpeg yt-dlp libasound2-dev libssl-dev pkg-config
```

**Arch Linux**:
```bash
sudo pacman -S ffmpeg yt-dlp alsa-lib openssl pkgconf
```

**macOS** (via Homebrew):
```bash
brew install ffmpeg yt-dlp
```

---

## Development Workflow

### 1. Fork & Clone

```bash
git clone https://github.com/shrijit37/tuna-tui.git
cd tuna-tui
```

### 2. Build & Run

```bash
# Build dev profile
cargo build

# Run locally
cargo run

# Build protocol-only (TXC) without the streaming backend
cargo run --no-default-features --features txc
```

### 3. Testing

Run the test suite before submitting changes:

```bash
# Run all unit and integration tests
cargo test --all-features

# Run a specific test
cargo test --test thumbs_queue_search
```

### 4. Code Quality & Formatting

We enforce strict formatting and clippy rules in CI:

```bash
# Check code formatting
cargo fmt --all --check

# Format code automatically
cargo fmt --all

# Run clippy with warnings treated as errors
cargo clippy --all-targets --all-features -- -D warnings
```

---

## Project Architecture

Tuna TUI is organized into modular components:

```
src/
├── app/          # App state, event loop, library models, settings menu, and disk persistence
├── audio/        # Audio visualizer, FFT analysis, spectrum smoothing
├── browse/       # Library browsing, search results, artist/album drill-ins
├── engine/       # Streaming playback engine (yt-dlp -> ffmpeg -> rodio), stream reconnects
├── input/        # Keyboard handling, mouse gestures, media keys (MPRIS)
├── lyrics/       # LRC parser, lrclib client, YouTube Music fallback, transliteration
├── providers/    # InnerTube (YouTube Music) client, yt-dlp process manager
├── txc/          # TXC theme and terminal color protocol (can be used standalone)
├── ui/           # Ratatui widgets: Now Playing, Queue, Lyrics, Library, Settings
├── config.rs     # Configuration parsing and TOML serialization
└── main.rs       # Entrypoint, terminal raw mode init, Tokio runtime orchestration
```

---

## Submitting Pull Requests

1. **Create a topic branch** from `master`:
   ```bash
   git checkout -b feature/my-new-feature
   ```
2. **Keep changes focused**: One feature or bugfix per PR.
3. **Add tests**: If adding new functionality, include unit or integration tests in `tests/` or `src/main_tests/`.
4. **Follow commit conventions**: Use clear, descriptive commit messages.
5. **Verify CI**: Ensure `cargo test`, `cargo fmt`, and `cargo clippy` pass locally before pushing.

---

## Reporting Issues

- **Bug Reports**: Use the [Bug Report template](.github/ISSUE_TEMPLATE/bug_report.yml). Include your OS, terminal emulator, `yt-dlp --version`, and reproduction steps.
- **Feature Requests**: Use the [Feature Request template](.github/ISSUE_TEMPLATE/feature_request.yml) to discuss ideas before implementing large changes.

---

## License

By contributing to Tuna TUI, you agree that your contributions will be licensed under the project's [MIT License](LICENSE).
