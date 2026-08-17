//! The binary's unit tests, kept in the crate (not `tests/`) because they
//! exercise items that are private to `main.rs`.

mod meta_cache;
mod nav;
mod playlist;
mod search;
mod sync;

/// Live-API tests, `#[ignore]`d so `cargo test` stays offline:
///
///     cargo test --bin tuna-tui -- --ignored --nocapture
///
/// They catch YouTube/yt-dlp transport drift (bot-gates, throttling — the
/// standing `Myx-jqp` risk) changing endpoints out from under the player.
mod live;
