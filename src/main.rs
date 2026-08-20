//! tuna-tui — the fully-wired terminal music player.
//!
//! yt-dlp → ffmpeg → rodio streaming engine + album-art-reactive theming with
//! cross-fades + live FFT visualizer, in noodle's visual language. Local
//! library (likes / follows / saves / play history) plus YouTube search,
//! playlists, channels, radio — shuffle, repeat, and a live queue view.

/// The context actions menu and its (fully local) effects.
mod actions;
/// The application state. `ui` reads it, `input` writes it, `browse` feeds it
/// over channels. Lives in the binary (not the library) because it is what
/// this binary is.
mod app;
/// The browse surface: library sections, search, drill-ins — out of the local
/// store + rolling history and the yt-dlp CLI. Lives in the binary (not the
/// library) because it speaks the model types defined here.
mod browse;
/// The input layer. Turns terminal and media-key events into `App` mutations
/// and channel sends — the one layer that writes state.
/// Lives in the binary (not the library) because it mutates `App`, which is here.
mod input;
/// The render tree. Reads `App`, writes `FrameOut`; never the other way round.
/// Lives in the binary (not the library) because it needs `App`, which is here.
mod ui;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MediaKeyCode, MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::buffer::CellDiffOption;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;
use ratatui_image::picker::Picker;

use actions::*;
use app::*;
use browse::*;
use input::*;
use tuna_tui::anim::ThemeFade;
use tuna_tui::audio::NUM_BANDS;
use tuna_tui::components::{gradient_line, left_bar_block};
use tuna_tui::cover::Cover;
use tuna_tui::engine::{self, Engine, EngineEvent};
use tuna_tui::gradient::{self};
use tuna_tui::liblog::{install_tuna_log, liblog};
use tuna_tui::term::{acquire_single_instance_lock, init_terminal, restore_terminal, Term};
use tuna_tui::theme::{Theme, TOKYONIGHT};
use tuna_tui::util::{center_v, fmt_ms, truncate, vol_u16};
use ui::render;

use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};

// ------------------------------------------------------------------ main

fn main() -> Result<()> {
    // `tuna-tui theme …` is a socket client, not a player: it must not start the
    // engine or touch the terminal. Intercepting argv here — before anything
    // else in `main` runs — is what guarantees that, and it also keeps `theme`
    // from reaching the "first positional argument is a URI" path in `boot`.
    #[cfg(all(feature = "txc", unix))]
    {
        let argv: Vec<String> = std::env::args().collect();
        if argv.get(1).is_some_and(|a| a == "theme") {
            std::process::exit(tuna_tui::txc::cli::run(&argv[2..]));
        }
    }

    // House the renamed config/cache dirs before anything opens them (the log
    // below, then the lock, then the session snapshot).
    tuna_tui::config::migrate_legacy_paths();
    install_tuna_log();

    // Refuse to start a second instance — two tuna-tui instances would race on
    // the persisted state file.
    let _instance_lock = acquire_single_instance_lock();

    // Restore last session first, so the engine starts at the saved volume.
    let saved = SavedState::load();
    let init_vol = if saved.volume == 0 {
        80
    } else {
        saved.volume.min(100)
    };

    // No OAuth anymore: the local library + yt-dlp need no credentials, so the
    // terminal is taken over directly.
    let terminal = init_terminal()?;

    // Query the terminal for its graphics protocol before anything else is
    // running: picking sixel swaps `TERM` around the query, and `setenv` is only
    // safe without concurrent readers. Hence the hand-built runtime below rather
    // than `#[tokio::main]`, which would already have spawned its workers by the
    // time this line ran.
    let picker = Cover::make_picker(tuna_tui::config::get().protocol.as_deref());
    // Halfblocks here means the graphics query got no answer — the art will look
    // like a 25×26 mosaic. TUNA_PROTOCOL overrides it.
    liblog(format!(
        "cover: {:?}, font {:?}",
        picker.protocol_type(),
        picker.font_size()
    ));

    #[cfg(target_os = "macos")]
    let res = run_player_macos(terminal, saved, init_vol, picker);
    #[cfg(not(target_os = "macos"))]
    let res = run_player(terminal, saved, init_vol, picker, true);
    res
}

fn run_player(
    mut terminal: Term,
    saved: SavedState,
    init_vol: u8,
    picker: Picker,
    media_platform_ready: bool,
) -> Result<()> {
    // Building reqwest's blocking client creates and drops its own inner
    // runtime, which tokio refuses inside a live runtime — construct it here,
    // before ours starts (see `httpcache::blocking_client`).
    tuna_tui::httpcache::warm_blocking_client();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("start tokio runtime")?;
    let outcome = runtime.block_on(boot(
        &mut terminal,
        saved,
        init_vol,
        picker,
        media_platform_ready,
    ));
    let restored = restore_terminal(&mut terminal);
    // Say goodbye *after* the screen is back, so a subscriber that has stopped
    // reading can never hold the alternate screen open while `shutdown` waits
    // on it. On the error path the publisher was dropped inside `boot`, and
    // `Publisher`'s `Drop` sends the same `bye` — this call exists so that the
    // clean path does not depend on drop order.
    let res = match outcome {
        Ok(handle) => {
            shutdown_publisher(handle);
            Ok(())
        }
        Err(e) => Err(e),
    };
    restored?;
    res
}

/// macOS delivers Now Playing commands (AirPods, Control Center) through the
/// main thread's run loop, so the winit loop takes the main thread and the
/// player runs beside it. No loop only costs the native integration.
#[cfg(target_os = "macos")]
fn run_player_macos(terminal: Term, saved: SavedState, init_vol: u8, picker: Picker) -> Result<()> {
    use winit::application::ApplicationHandler;
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
    use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};

    struct PlayerDone;

    struct MediaPump;
    impl ApplicationHandler<PlayerDone> for MediaPump {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
        fn window_event(
            &mut self,
            _: &ActiveEventLoop,
            _: winit::window::WindowId,
            _: winit::event::WindowEvent,
        ) {
        }
        fn user_event(&mut self, event_loop: &ActiveEventLoop, _: PlayerDone) {
            event_loop.exit();
        }
    }

    // Accessory keeps tuna-tui out of the Dock and the app switcher.
    let event_loop = match EventLoop::<PlayerDone>::with_user_event()
        .with_activation_policy(ActivationPolicy::Accessory)
        .build()
    {
        Ok(event_loop) => event_loop,
        Err(e) => {
            liblog(format!("media event loop unavailable: {e}"));
            return run_player(terminal, saved, init_vol, picker, false);
        }
    };

    let proxy = event_loop.create_proxy();
    let player = std::thread::spawn(move || {
        let res = run_player(terminal, saved, init_vol, picker, true);
        let _ = proxy.send_event(PlayerDone);
        res
    });

    if let Err(e) = event_loop.run_app(&mut MediaPump) {
        liblog(format!("media event loop stopped: {e}"));
    }
    match player.join() {
        Ok(res) => res,
        Err(_) => Err(anyhow::anyhow!("player thread panicked")),
    }
}

/// What `boot` hands back so `main` can say goodbye on the exit path.
///
/// A type alias rather than `#[cfg]` on the signature: the non-TXC build then
/// differs in exactly one place instead of in every function that carries the
/// value through.
#[cfg(all(feature = "txc", unix))]
type TxcHandle = Option<tuna_tui::txc::publish::Publisher>;
#[cfg(not(all(feature = "txc", unix)))]
type TxcHandle = ();

/// Send `bye` to every subscriber and close the socket.
#[cfg(all(feature = "txc", unix))]
fn shutdown_publisher(handle: TxcHandle) {
    if let Some(publisher) = handle {
        publisher.shutdown(tuna_tui::txc::ByeReason::Shutdown);
    }
}

#[cfg(not(all(feature = "txc", unix)))]
fn shutdown_publisher(_handle: TxcHandle) {}

/// Bind the TXC theme socket, or run without one.
///
/// **Publishing is opt-out.** Album-reactive colour is a headline feature, so
/// it is on unless `TUNA_NO_COLOR_SOCKET` is set to something other than `0` or
/// the empty string.
///
/// A bind failure is never fatal — not a stale socket, not a read-only
/// `XDG_RUNTIME_DIR`, not an exhausted thread limit. Tuna TUI is a music player
/// first; losing colour publishing costs a subscriber a repaint, whereas
/// refusing to start costs the user their music. Failures go to the tuna-tui log,
/// where the rest of the optional-integration diagnostics already live.
#[cfg(all(feature = "txc", unix))]
fn bind_publisher() -> TxcHandle {
    if std::env::var("TUNA_NO_COLOR_SOCKET").is_ok_and(|v| !v.is_empty() && v != "0") {
        liblog("txc: TUNA_NO_COLOR_SOCKET set; colour publishing disabled");
        return None;
    }
    let path = tuna_tui::txc::socket_path();
    match tuna_tui::txc::publish::Publisher::bind(&path) {
        Ok(publisher) => {
            liblog(format!("txc: publishing on {}", path.display()));
            Some(publisher)
        }
        Err(e) => {
            liblog(format!(
                "txc: could not bind {} ({e}); continuing without colour publishing",
                path.display()
            ));
            None
        }
    }
}

fn optional_integration<T, E>(ready: bool, init: impl FnOnce() -> Result<T, E>) -> Option<T> {
    ready.then(init).and_then(Result::ok)
}

fn should_restore_saved_playback(
    configured: bool,
    startup_uri: Option<&str>,
    saved: &SavedState,
) -> bool {
    configured && startup_uri.is_none() && saved.last_played.is_some()
}

/// Everything from the loading screen to the event loop. Split out of `main` so
/// a failure on the way up still leaves the terminal restored.
async fn boot(
    terminal: &mut Term,
    saved: SavedState,
    init_vol: u8,
    picker: Picker,
    media_platform_ready: bool,
) -> Result<TxcHandle> {
    // The engine's in-band metadata channel. Established before the engine
    // starts so no event can land on a missing sender; boot passes the receiver
    // on to `run_ui`, where it feeds `apply_meta`. Bounded with drop-oldest
    // (F25): each message can carry a multi-MB cover image, so a momentarily
    // busy UI must shed the OLDEST pending message instead of queueing images
    // without bound — and saturation never blocks the engine's meta worker.
    let (engine_meta_tx, engine_meta_rx) = flume::bounded::<tuna_tui::engine::EngineMeta>(4);
