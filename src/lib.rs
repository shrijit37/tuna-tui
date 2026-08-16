//! tuna-tui — a lean, beautiful terminal music player.
//!
//! FE: the design-token system (noodle's visual language) ported to ratatui,
//! plus album-art-reactive theming with cross-fades.
//! Backend (`streaming` feature): a yt-dlp → ffmpeg → rodio engine with a tee'd
//! FFT visualizer and real track-change events.

use std::path::PathBuf;

pub mod anim;
pub mod color;
pub mod components;
pub mod cover;
pub mod gradient;
pub mod httpcache;
pub mod liblog;
pub mod lyrics;
pub mod reactive;
pub mod theme;
pub mod util;

#[cfg(all(feature = "txc", unix))]
pub mod txc;

#[cfg(feature = "streaming")]
pub mod audio;
#[cfg(feature = "streaming")]
pub mod config;
#[cfg(feature = "streaming")]
#[cfg(feature = "streaming")]
pub mod term;
#[cfg(feature = "streaming")]
pub mod yt;

/// Cross-platform home directory. Uses `HOME` on Unix, `USERPROFILE` on Windows.
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    let var = "HOME";
    #[cfg(windows)]
    let var = "USERPROFILE";
    std::env::var(var).ok().map(PathBuf::from)
}
