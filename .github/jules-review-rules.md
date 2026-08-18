# Review rules for shrijit37/tuna-tui

Tuna TUI — a lean terminal music player in Rust (ratatui TUI, yt-dlp → ffmpeg → rodio streaming, MIT). The Spotify→YouTube port is landed; there is deliberately zero Spotify/OAuth code left in the tree.

## Always blocking

- PR body must end with a complete **"Architecture summary"** section: what was executed (files, functions, channel/flag shapes), what problem it solves, and the behavioral deltas.
- PR body must reference the relevant bead id(s) (`Myx-*`) and audit finding id(s) (e.g. `F18`) early in the body.
- Violations of the binding safe fixes / regression cautions in `docs/perf-audit-2026-08-17.md`.
- Anything that reintroduces Spotify/OAuth (`client_id`, `spotify:` URIs) or re-adds deleted modules (`src/webapi.rs`, `src/api/*`, `src/hybrid_expander.rs`).

## Always warn

- Edits to the UI layer that CLAUDE.md marks keep-untouched (`src/ui/*`, `src/app/*` except `persist.rs`, `src/input/*`, `src/cover.rs`, `src/theme.rs`, `src/color.rs`, `src/gradient.rs`, `src/anim.rs`, `src/reactive.rs`) without the PR stating the purpose requires it.
- New typed serde structs where the house style is untyped yt-dlp `-J` JSON-path reads.
- Hand-editing `Cargo.lock` (cargo-managed — use `cargo update`).
- New network-touching tests not marked `#[ignore]` (live tests need network + yt-dlp).

## Build gates (CI also runs these)

- `cargo fmt --all --check`, `clippy --all-targets --all-features` under `RUSTFLAGS="-D warnings"`, and `cargo test --all-features` must pass.

## What to skip

- Pre-existing issues in lines this PR did not modify.
- Pedantic nits a senior engineer wouldn't raise.
