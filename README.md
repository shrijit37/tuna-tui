# Tuna TUI

A lean, beautiful terminal music player. Rust + [ratatui], playing YouTube
through `yt-dlp → ffmpeg → rodio`, with local library, lyrics, cover-art theme
fades, a spectrum visualizer, and media-key/MPRIS support. MIT licensed.

[ratatui]: https://ratatui.rs

## Features

- **Search** YouTube (`ytsearchN:` flat results) and play straight from the results
- **Home** — rolling history of what you actually played (play counts + recency)
- **Library** — liked tracks, albums, artists, and playlists persisted locally;
  external playlists and album slugs drill in through YouTube
- **Resilient playback engine** — a watchdog re-resolves stalled or dropped
  streams (googlevideo connections die mid-stream on some networks; the engine
  recovers instead of stalling)
- **Lyrics** from lrclib.net, keyed on artist/title/album/duration
- **Cover art + adaptive theme fade**, generated from each track's artwork
- **Radio** from any track, with a fallback chain for fresh/obscure seeds
- **Queue** view that mirrors the playing list; add-to-queue from the actions menu
- **Volume / shuffle / repeat / seek** — local mixer and queue operations
- **Resume on startup** — the session snapshot (`state.json`) restores your
  context and seek position
- **Media keys / MPRIS** (souvlaki), **zen mode**, instant **actions menu**,
  per-column **sort**, and a **TXC color protocol** (pure data types + color
  math, usable standalone without the streaming backend)
- **FFT visualizer** fed from what is actually served to the audio device

## Requirements

- `yt-dlp` and `ffmpeg` **on `PATH`** — the app shells out to both
  (search/resolve and stream decode). Nothing plays without them.
- A terminal with a tty (ratatui/crossterm; CSI-u terminals like foot,
  kitty, or WezTerm unlock `Shift+Enter` to play the selected item directly).

## Install

| Method | Command / notes |
|---|---|
| cargo (crates.io) | `cargo install tuna-tui` — requires `yt-dlp` + `ffmpeg` installed separately |
| Nix | `nix run github:shrijit37/tuna-tui` (flake: dev shell + build) |
| Debian / Ubuntu | `.deb` from the GitHub release — declares `libasound2`, `libssl3`, `yt-dlp`, `ffmpeg` |
| Homebrew | `brew install shrijit37/homebrew-tap/tuna-tui` — formula declares `yt-dlp` and `ffmpeg` |
| Arch (AUR) | `tuna-tui` package — depends on `yt-dlp`, `ffmpeg`, `alsa-lib`, `openssl` |
| From source | `cargo build --release` (default features `streaming` + `txc`); `--no-default-features --features txc` builds the protocol-only half |

The binary is self-contained except for `yt-dlp`/`ffmpeg` and the system
libraries it links (`libasound2` for audio output, `libssl3` for HTTPS).

## Usage

Run `tuna-tui` in a terminal. The left pane is the library (Home / Liked /
Albums / Artists / Playlists), the right pane shows Now Playing, Queue, or
Lyrics, and the footer holds transport state.

| Keys | Action |
|---|---|
| `j` / `k`, `↑` / `↓` | move selection |
| `Enter` | open / drill in (playlist, album, artist); in search, run the search |
| `Shift+Enter` | play the selected item outright (needs a CSI-u terminal) |
| `Tab` / `]`, `Shift+Tab` / `[` | next / previous library section |
| `←` / `→` | rotate the right-pane view (Now Playing / Queue / Lyrics) |
| `Shift+←` / `Shift+→` | seek −5 s / +5 s |
| `/` | search (type, `Enter` to run, `Esc` to cancel; `Ctrl+U` clears) |
| `Space` / `p` | play / pause (or resume your last context on a fresh start) |
| `n` / `b` | next / previous track |
| `+` / `=` , `-` / `_` | volume up / down (5 steps) |
| `s` | toggle shuffle · `S` shuffle and play the selection |
| `R` | toggle repeat · `r` reload the library |
| `P` | play the highlighted playlist / album / artist directly |
| `a` | actions menu on the selection (like, add to queue, copy link, open…) |
| `o` | cycle sort for the current list |
| `z` | zen mode (hide the library) |
| `q` | quit · `Ctrl+C` twice also quits |
| `Esc` | back one level |
| media keys | play/pause, next, previous, stop, volume |

## Configuration and data

- `~/.config/tuna-tui/config.toml` — user settings: `search_limit`,
  `audio_format`, and an optional `cookies_file` for `yt-dlp --cookies`
  (Netscape format; quiets the bot checks on throttled networks and unlocks
  private playlists). The `TUNA_PROTOCOL` environment variable overrides the
  protocol for URL building.
- `~/.cache/tuna-tui/state.json` — session snapshot: playback context, store
  (liked / albums / artists / playlists / history), and last seek position.
  Items are `tuna:` URIs; legacy `myx:` rows from before the rebrand still
  parse. One-time migration moves the old `~/.config/myx` / `~/.cache/myx`
  dirs to the `tuna-tui` names (log with `TUNA_LOG=1`).

## Known limits

- YouTube streaming is a ToS grey zone: anonymized traffic can hit bot checks
  (set `cookies_file`); the stream leg pins `player_client=android`, which is
  the verified-unthrottle mitigation on at least one box, and the watchdog
  recovers from per-connection drops.
- No first-class album/artist entities on YouTube — those live in your local
  library; album drill-in is a slug search.
- Seek restarts the stream (`-ss`, ~1 s). Multi-device Connect had no YouTube
  equivalent and was dropped with the Spotify backend.

## Development

`examples/probe` drives the `Expander` (yt-dlp resolve + ffmpeg decode);
`theme_demo` and `txc_demo` exercise the theme and color protocol.
CI runs fmt/clippy (warnings denied)/tests on Linux, macOS, and Windows;
live-network tests are `#[ignore]`d.

## License

MIT — see [LICENSE](LICENSE) and [NOTICE](NOTICE). Copyright (c) 2026 Haseeb Khalid, Shrijit Srivastava.
