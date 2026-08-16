//! `txc_demo` — an external process that recolors itself live from TXC.
//!
//! The real implementation lives in `txc_demo_support/imp.rs`. This file exists
//! only to keep the demo off non-Unix platforms.
//!
//! TXC is an `AF_UNIX` protocol, so the demo cannot exist on Windows — and
//! Cargo has no way to target-gate an `[[example]]`. Without this split,
//! `cargo test` and `cargo clippy --all-targets` would fail to compile on
//! Windows even though the player itself builds there fine.
//!
//! The support directory has no `main.rs`, so Cargo does not auto-discover it
//! as a second example. That is deliberate — it lets `dump_theme` keep being
//! discovered normally, which setting `autoexamples = false` would have
//! quietly broken.
//!
//! ```text
//! cargo run --example txc_demo                 # $XDG_RUNTIME_DIR/tuna-tui/theme.sock
//! cargo run --example txc_demo /tmp/my.sock    # explicit path
//! cargo run --example txc_demo -- --fake       # no Tuna TUI required
//! ```

#[cfg(unix)]
#[path = "txc_demo_support/imp.rs"]
mod imp;

#[cfg(unix)]
fn main() {
    imp::main();
}

#[cfg(not(unix))]
fn main() {
    eprintln!("txc_demo: TXC is Unix-only — it needs an AF_UNIX socket.");
    std::process::exit(1);
}
