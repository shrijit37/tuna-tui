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

    // The pure-YouTube expander: every uri the app produces is `yt:` now, so
    // there is nothing for a hybrid bridge to do.
    let expander: Arc<dyn tuna_tui::engine::Expander> =
        Arc::new(tuna_tui::engine::YtExpander::default());

    let (ev_tx, ev_rx) = flume::unbounded::<EngineEvent>();
    let engine = engine::run(
        ev_tx,
        engine_meta_tx,
        engine_meta_rx.clone(),
        init_vol,
        expander,
    )
    .context("start engine")?;

    // The one positional argument is a yt: URI (or bare YouTube URL/playlist).
    // It always wins over a
    // persisted session; `theme` never reaches here (see `main`).
    let startup_uri = std::env::args().nth(1).filter(|a| a != "theme");
    if let Some(uri) = startup_uri.as_ref() {
        let _ = engine.play_context(uri.clone(), false);
        // The URI path never sees a `Playing`-handler reapply (the boot is
        // marked started already) — hand the persisted modes over now.
        let _ = engine.shuffle(saved.shuffle);
        let _ = engine.repeat(saved.repeat);
    }

    let restore_on_startup = should_restore_saved_playback(
        tuna_tui::config::get().restore_on_startup,
        startup_uri.as_deref(),
        &saved,
    );
    let now = restore_on_startup
        .then_some(saved.last_played.as_ref())
        .flatten()
        .map(|last_played| NowPlaying {
            uri: last_played.uri.clone(),
            title: last_played.title.clone(),
            artist: last_played.artist.clone(),
            album: last_played.album.clone(),
            duration_ms: last_played.duration_ms,
            position_ms: last_played.position_ms,
            position_at: Instant::now(),
            is_playing: false,
            cover: None,
        });
    let restore_uri = now.as_ref().map(|track| track.uri.clone());
    let (queue, queue_uris, source, source_name) = if restore_on_startup {
        (
            saved.queue,
            saved.queue_uris,
            saved.source,
            saved.source_name,
        )
    } else {
        (Vec::new(), Vec::new(), PlaySource::default(), String::new())
    };

    // HWND is a Windows-specific API.
    #[cfg(unix)]
    let hwnd = None;

    // Tuna TUI is a TUI with no window of its own, get the console's window instead.
    #[cfg(windows)]
    let hwnd = Some(unsafe { windows_win::sys::GetConsoleWindow() });

    let media_controls = optional_integration(media_platform_ready, || {
        MediaControls::new(PlatformConfig {
            dbus_name: "tuna-tui",
            display_name: "Tuna TUI",
            hwnd,
        })
    });
    if media_platform_ready && media_controls.is_none() {
        liblog("media controls unavailable; continuing without native integration");
    }

    let app = App {
        svc: Services { engine, picker },
        media_controls,
        #[cfg(all(feature = "txc", unix))]
        txc: bind_publisher(),
        playback: PlaybackState {
            last_advance: None,
            now,
            seek_target: None,
            seek_last_step: Instant::now(),
            seek_last_input: Instant::now(),
        },
        theme: ThemeState {
            displayed: TOKYONIGHT,
            target: TOKYONIGHT,
            fade: None,
        },
        status: "loading library…".to_string(),
        browse: BrowseState {
            library: Library::default(),
            section: Section::Home,
            selected: 0,
            sort: SortMode::Added,
            details: Vec::new(),
            playlist_input: None,
        },
        transport: Transport {
            shuffle: saved.shuffle,
            repeat: saved.repeat,
            volume: if saved.volume == 0 {
                80
            } else {
                saved.volume.min(100)
            },
            queue,
            queue_uris,
            playback_started: startup_uri.is_some(),
            source,
            source_name,
        },
        search: SearchState {
            input_mode: false,
            input: Default::default(),
            searching: false,
            in_flight: false,
            search_results: Vec::new(),
        },
        view: ViewState {
            mode: RightView::NowPlaying,
            zen: tuna_tui::config::get().zen_default,
            lyrics: Vec::new(),
            lyrics_synced: false,
            actions: None,
            settings: None,
            queue_selected: 0,
        },
        config: tuna_tui::config::get().clone(),
        session: SessionState {
            restore_uri,
            pending_meta: None,
            last_ctrl_c: None,
            last_click: None,
            radio_in_flight: false,
            meta_cache: {
                let mut cache = std::collections::HashMap::new();
                if let Some(last) = &saved.last_played {
                    if !last.title.is_empty() {
                        cache.insert(last.uri.clone(), (last.title.clone(), last.artist.clone()));
                    }
                }
                for h in &saved.store.history {
                    if !h.title.is_empty() {
                        cache.insert(h.uri.clone(), (h.title.clone(), h.artist.clone()));
                    }
                }
                for l in &saved.store.liked {
                    if !l.name.is_empty() {
                        cache.insert(l.uri.clone(), (l.name.clone(), l.subtitle.clone()));
                    }
                }
                cache
            },
        },
        store: saved.store.clone(),
        store_dirty: false,
        queue_dirty: false,
        art_repaint: ArtRepaint::Idle,
    };

    run_ui(terminal, app, ev_rx, engine_meta_rx).await
}

struct Radio {
    start_position_ms: u32,
    uris: Vec<String>,
    meta: Vec<(String, String, String)>,
}

/// Every `Sender` the UI loop hands to input handlers and spawned fetches.
/// Receivers stay local to `run_ui` because `select!` needs them there.
///
/// The menu, action-status and live-queue channels died with the Spotify API:
/// the menu is instant (`build_action_menu`), actions write locally, and the
/// queue renders the engine's loaded list.
pub(crate) struct UiChannels {
    pub(crate) lib: flume::Sender<(Section, Vec<LibItem>)>,
    pub(crate) search: flume::Sender<Vec<LibItem>>,
    pub(crate) suggest: flume::Sender<String>,
    pub(crate) lyrics: flume::Sender<(Vec<(u32, String)>, bool)>,
    pub(crate) detail: flume::Sender<(String, String, Vec<LibItem>)>,
    pub(crate) radio: flume::Sender<Result<Radio, String>>,
}

/// Should the 24s sync tick re-run `refresh_local_queue`?
///
/// The refresh is the only mechanism that upgrades raw-URI queue rows to
/// "title — artist" as `EngineMeta` lands one track at a time, and the only
/// re-sync after recovery-removal and resume-restore — so it must run when
/// the engine queue or the metadata cache changed length. The `usize::MAX`
/// sentinel makes the first tick after launch always refresh (covering the
/// resume-restore path, where the lengths can already be in steady state).
fn refresh_needed(qlen: usize, mlen: usize, last_q: usize, last_m: usize) -> bool {
    qlen != last_q || mlen != last_m
}

async fn run_ui(
    terminal: &mut Term,
    mut app: App,
    ev_rx: flume::Receiver<EngineEvent>,
    engine_meta_rx: flume::Receiver<tuna_tui::engine::EngineMeta>,
) -> Result<TxcHandle> {
    let (in_tx, in_rx) = flume::unbounded::<Event>();
    std::thread::spawn(move || loop {
        if matches!(event::poll(Duration::from_millis(200)), Ok(true)) {
            if let Ok(ev) = event::read() {
                if in_tx.send(ev).is_err() {
                    break;
                }
            }
        }
    });

    let (lib_tx, lib_rx) = flume::unbounded::<(Section, Vec<LibItem>)>();
    let (search_tx, search_rx) = flume::unbounded::<Vec<LibItem>>();
    let (suggest_tx, suggest_rx) = flume::unbounded::<String>();
    let (suggestions_tx, suggestions_rx) = flume::unbounded::<Vec<LibItem>>();
    let (lyrics_tx, lyrics_rx) = flume::unbounded::<(Vec<(u32, String)>, bool)>();
    let (detail_tx, detail_rx) = flume::unbounded::<(String, String, Vec<LibItem>)>();
    let (radio_tx, radio_rx) = flume::unbounded::<Result<Radio, String>>();
    let (souvlaki_tx, souvlaki_rx) = flume::unbounded::<MediaControlEvent>();
    let chans = UiChannels {
        lib: lib_tx,
        search: search_tx,
        suggest: suggest_tx,
        lyrics: lyrics_tx,
        detail: detail_tx,
        radio: radio_tx,
    };
    spawn_library_fetch(app.store.clone(), chans.lib.clone());
    browse::spawn_suggestions(suggest_rx, suggestions_tx);

    if app.playback.now.is_some() {
        resume_source(&mut app, &chans.radio);
        app.transport.playback_started = true;
    }

    // Book the pending guard for the restored last-played track: its metadata
    // arrives in-band once playback starts (`EngineMeta`), and the guard keeps
    // any older track's reply from overwriting it.
    if let Some(uri) = app.session.restore_uri.take() {
        app.session.pending_meta = Some(uri);
    }

    if let Some(controls) = app.media_controls.as_mut() {
        if controls
            .attach(move |event| {
                let _ = souvlaki_tx.send(event);
            })
            .is_err()
        {
            liblog("media controls failed to attach; continuing without native integration");
            app.media_controls = None;
        }
    }
    let mut media_events_open = true;

    // A persistent interval must live OUTSIDE the select loop. Recreating a
    // `sleep()` every loop starves forever when player events are continuously
    // ready: the future gets cancelled/reset before its deadline. That was the
    // frozen-UI bug.
    let mut frame = tokio::time::interval(Duration::from_millis(8));
    frame.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_draw = Instant::now() - IDLE_REDRAW;
    let mut last_sync = Instant::now();
    // Last observed engine-queue / metadata-cache lengths for the sync tick's
    // refresh gate. The `usize::MAX` sentinel forces a refresh on the first
    // tick after launch — the resume-restore path needs it even when the
    // lengths are already in steady state.
    let mut last_queue_len = usize::MAX;
    let mut last_meta_len = usize::MAX;
    // Last-saved transport fields for the F21 save gate: a stopped-session
    // mixer tweak (volume/shuffle/repeat — mutated in the protected input
    // files) must still persist within the 24s cadence, so the tick compares
    // the live fields against what the previous save wrote.
    let mut last_saved_volume = app.transport.volume;
    let mut last_saved_shuffle = app.transport.shuffle;
    let mut last_saved_repeat = app.transport.repeat;
    // Nothing is on screen yet, so the first tick must draw.
    let mut dirty = true;
    let mut last_layout = (app.view.mode, app.view.zen);
    let mut overlay_open = app.view.actions.is_some();
    // What the renderer writes. Lives across frames: the hit rects are what the
    // mouse handler reads between draws, and `lib_offset` is fed back into the
    // next frame's sticky-viewport calculation.
    let mut out = FrameOut::default();

    loop {
        let touched = tokio::select! {
            biased;
            _ = frame.tick() => {
                app.playback.flush_seek(&app.svc.engine, Instant::now());
                // Drain library updates deterministically before rendering. Keeping
