//! Lyrics support: LRC parsing (pure) and lrclib fetching.

/// lrclib fetch (needs the streaming backend's HTTP stack).
#[cfg(feature = "streaming")]
pub mod fetch;
pub mod parse;
