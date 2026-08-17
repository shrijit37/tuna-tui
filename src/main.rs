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

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
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
    // on to `run_ui`, where it feeds `apply_meta`.
    let (engine_meta_tx, engine_meta_rx) = flume::unbounded::<tuna_tui::engine::EngineMeta>();

    // The pure-YouTube expander: every uri the app produces is `yt:` now, so
    // there is nothing for a hybrid bridge to do.
    let expander: Arc<dyn tuna_tui::engine::Expander> = Arc::new(tuna_tui::engine::YtExpander);

    let (ev_tx, ev_rx) = flume::unbounded::<EngineEvent>();
    let engine = engine::run(ev_tx, engine_meta_tx, init_vol, expander).context("start engine")?;

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
            zen: false,
            lyrics: Vec::new(),
            lyrics_synced: false,
            actions: None,
        },
        session: SessionState {
            restore_uri,
            pending_meta: None,
            last_ctrl_c: None,
            last_click: None,
            radio_in_flight: false,
            meta_cache: std::collections::HashMap::new(),
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
}

/// Every `Sender` the UI loop hands to input handlers and spawned fetches.
/// Receivers stay local to `run_ui` because `select!` needs them there.
///
/// The menu, action-status and live-queue channels died with the Spotify API:
/// the menu is instant (`build_action_menu`), actions write locally, and the
/// queue renders the engine's loaded list.
struct UiChannels {
    lib: flume::Sender<(Section, Vec<LibItem>)>,
    search: flume::Sender<Vec<LibItem>>,
    lyrics: flume::Sender<(Vec<(u32, String)>, bool)>,
    detail: flume::Sender<(String, String, Vec<LibItem>)>,
    radio: flume::Sender<Result<Radio, String>>,
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
    let (lyrics_tx, lyrics_rx) = flume::unbounded::<(Vec<(u32, String)>, bool)>();
    let (detail_tx, detail_rx) = flume::unbounded::<(String, String, Vec<LibItem>)>();
    let (radio_tx, radio_rx) = flume::unbounded::<Result<Radio, String>>();
    let (souvlaki_tx, souvlaki_rx) = flume::unbounded::<MediaControlEvent>();
    let chans = UiChannels {
        lib: lib_tx,
        search: search_tx,
        lyrics: lyrics_tx,
        detail: detail_tx,
        radio: radio_tx,
    };
    spawn_library_fetch(app.store.clone(), chans.lib.clone());

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
    let mut frame = tokio::time::interval(Duration::from_millis(16));
    frame.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_draw = Instant::now() - IDLE_REDRAW;
    let mut last_sync = Instant::now();
    // Last observed engine-queue / metadata-cache lengths for the sync tick's
    // refresh gate. The `usize::MAX` sentinel forces a refresh on the first
    // tick after launch — the resume-restore path needs it even when the
    // lengths are already in steady state.
    let mut last_queue_len = usize::MAX;
    let mut last_meta_len = usize::MAX;
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
                // this solely as a select arm could starve under a hot player-event
                // stream / 60fps visualizer — which looked like a frozen library.
                let mut landed = false;
                while let Ok((section, mut items)) = lib_rx.try_recv() {
                    let count = items.len();
                    dirty = true;
                    landed = true;
                    liblog(format!("ui: received {} rows for {}", count, section.label()));
                    for (i, it) in items.iter_mut().enumerate() {
                        it.order = i as u32;
                    }
                    app.browse.library.set(section, items);
                    sort_list(app.browse.library.items_mut(section), app.browse.sort);
                    if section == app.browse.section {
                        app.normalize_selection();
                    }
                    app.status = format!("loaded {}", section.label());
                }
                // Local delivery cannot fail (the store is on disk, the sections are
                // built from it), so once the last section of a drain lands the
                // loading status clears — there is no retry or failure path anymore.
                // (The interim "loaded <section>" lines never render; the clear
                // happens in the same tick, before the frame is drawn.)
                if landed {
                    app.status.clear();
                }
                // Radio results are drained here (not as a `select!` arm) for the
                // same reason as the library: under the biased 16ms frame tick a
                // pure recv arm starves and the station never plays.
                while let Ok(rad) = radio_rx.try_recv() {
                    dirty = true;
                    // The resolve finished (or its timeout path failed): a
                    // fresh request can go out again.
                    app.session.radio_in_flight = false;
                    match rad {
                        Ok(radio) if !radio.uris.is_empty() => {
                            if let Err(e) = app.svc.engine.play_tracks(radio.uris, None, radio.start_position_ms, false) {
                                app.status = format!("couldn't play radio: {e:#}");
                            }
                            // Repeat/volume — deliberately not shuffle: the mix
                            // must keep its order.
                            push_transport_modes(&mut app);
                            app.transport.playback_started = true;
                            app.status = "radio started".to_string();
                            app.refresh_local_queue();
                        }
                        Ok(_) => {
                            app.status = "radio: no tracks returned".to_string();
                        }
                        Err(e) => {
                            app.status = format!("radio failed: {e}");
                        }
                    }
                }

                // The visualizer only animates while it is on screen; on Queue
                // its frame rate buys nothing. Synced lyrics move too — at the
                // idle rate the highlighted line lands half a second late.
                let animating = app.theme.fade.is_some()
                    || (app.view.mode == RightView::Lyrics && app.view.lyrics_synced)
                    || (app.view.mode == RightView::NowPlaying
                        && app.svc.engine.bands.try_lock().map(|g| g.is_active).unwrap_or(false));
                if app.art_repaint != ArtRepaint::Idle {
                    dirty = true;
                }
                if (app.view.mode, app.view.zen) != last_layout {
                    last_layout = (app.view.mode, app.view.zen);
                    app.art_repaint = ArtRepaint::Wipe;
                    dirty = true;
                }
                // An overlay draws over the art and the terminal loses those
                // pixels, so the cover has to be sent again once it closes.
                // Opening one must not wipe: the image would be redrawn a frame
                // later, back on top of the popup.
                let overlay = app.view.actions.is_some();
                if overlay != overlay_open {
                    overlay_open = overlay;
                    if !overlay {
                        app.art_repaint = ArtRepaint::Wipe;
                    }
                    dirty = true;
                }
                if should_draw(dirty, animating, last_draw.elapsed()) {
                    app.theme.advance();
                    // Present the frame atomically. Without this the terminal
                    // renders whatever has arrived so far, and a recolour that
                    // touches every glyph on screen shows up half-applied.
                    // Terminals that don't know the mode ignore it.
                    let _ = execute!(io::stdout(), BeginSynchronizedUpdate);
                    let repaint = app.art_repaint;
                    let drawn = terminal.draw(|f| render(f, &app, &mut out, repaint));
                    let _ = execute!(io::stdout(), EndSynchronizedUpdate);
                    drawn?;
                    app.art_repaint = app.art_repaint.advance();
                    last_draw = Instant::now();
                    dirty = false;
                }
                if last_sync.elapsed() >= SYNC_EVERY {
                    last_sync = Instant::now();
                    let qlen = app.svc.engine.queue_len();
                    let mlen = app.session.meta_cache.len();
                    // Refresh the local queue from the engine while playing so
                    // the snapshot stays current, then persist it (survives
                    // reboot). The write runs on a blocking thread — serializing
                    // the store + fs-write must not freeze the render loop.
                    //
                    // The refresh is gated on the queue / metadata-cache
                    // lengths changing: it re-formats every label, so at idle
                    // (nothing landing, no recovery-removal) it would only
                    // re-clone and re-format the same rows every 24s. `refresh_needed`
                    // fires on every metadata landing (label upgrade) and on
                    // recovery-removal (the engine snapshot shrinks).
                    if app.transport.playback_started
                        && refresh_needed(qlen, mlen, last_queue_len, last_meta_len)
                    {
                        app.refresh_local_queue();
                    }
                    last_queue_len = qlen;
                    last_meta_len = mlen;
                    // Dirty gate for the save: at idle the snapshot only
                    // changes while playing (position ticks) — and a playing
                    // transport keeps the save cadence on its own. Store
                    // mutations flag `store_dirty`; queue appends flag
                    // `queue_dirty`. When both are clean and playback is
                    // idle, skip the full-store clone + serialize + write.
                    let transport_dirty = app.transport.playback_started || app.queue_dirty;
                    if app.store_dirty || transport_dirty {
                        app.store_dirty = false;
                        app.queue_dirty = false;
                        let snapshot = save_state(&app);
                        tokio::task::spawn_blocking(move || snapshot.save());
                    }
                }
                false
            }
            ev = ev_rx.recv_async() => {
                let Ok(ev) = ev else { break };
                handle_engine_event(&mut app, ev);
                true
            }
            ev = in_rx.recv_async() => {
                match ev {
                    Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                        let quit = handle_key(&mut app, key.code, key.modifiers, &chans);
                        if quit {
                            // The last save must land before exit — await it.
                            let snapshot = save_state(&app);
                            let _ = tokio::task::spawn_blocking(move || snapshot.save()).await;
                            break;
                        }
                    }
                    Ok(Event::Mouse(m)) => {
                        let quit = handle_mouse(&mut app, &out, m, &chans);
                        if quit {
                            // The last save must land before exit — await it.
                            let snapshot = save_state(&app);
                            let _ = tokio::task::spawn_blocking(move || snapshot.save()).await;
                            break;
                        }
                    }
                    // Resizes lose inline art. Focus only does so when tmux
                    // repaints a pane; compositor focus-follows-mouse events do
                    // not and must not make the cover flash.
                    Ok(Event::Resize(..)) => {
                        app.art_repaint = ArtRepaint::Wipe;
                    }
                    Ok(Event::FocusGained) if std::env::var_os("TMUX").is_some() => {
                        app.art_repaint = ArtRepaint::Wipe;
                    }
                    _ => {}
                }
                true
            }
            ev = souvlaki_rx.recv_async(), if media_events_open => {
                match consume_media_event(ev, &mut media_events_open) {
                    Some(ev) => handle_media_control_event(&mut app, ev, &chans.radio),
                    None => {
                        app.media_controls = None;
                        liblog("media controls event channel closed; native integration disabled");
                    }
                }
                true
            }
            // In-band engine metadata: the only TrackMeta source left. The engine
            // already fetched the cover + theme; map onto the app's TrackMeta
            // and let the usual pipeline take over.
            em = engine_meta_rx.recv_async() => {
                if let Ok(em) = em {
                    apply_meta(
                        &mut app,
                        TrackMeta {
                            uri: em.uri,
                            title: em.title,
                            artist: em.artist,
                            album: em.album,
                            duration_ms: em.duration_ms,
                            image: TrackImage {
                                url: em.image_url,
                                image: em.image,
                            },
                            theme: em.theme,
                        },
                        &chans.lyrics,
                    );
                }
                true
            }
            s = search_rx.recv_async() => {
                if let Ok(results) = s {
                    app.search.in_flight = false;
                    app.search.search_results = results;
                    app.browse.selected = app.first_selectable();
                    app.status = if app.search.search_results.is_empty() {
                        "no results".to_string()
                    } else {
                        String::new()
                    };
                }
                true
            }
            ly = lyrics_rx.recv_async() => {
                if let Ok((lines, synced)) = ly {
                    app.view.lyrics = lines;
                    app.view.lyrics_synced = synced;
                }
                true
            }
            d = detail_rx.recv_async() => {
                if let Ok((context_uri, title, items)) = d {
                    app.browse.details.push(Detail { context_uri, title, items, parent_selected: app.browse.selected });
                    app.browse.selected = app.first_selectable();
                    app.status.clear();
                }
                true
            }
        };
        dirty |= touched;
    }
    // Hand the publisher back to `main` so the `bye` goes out on the same path
    // that restores the terminal, rather than relying on where `App` happens
    // to be dropped.
    #[cfg(all(feature = "txc", unix))]
    {
        Ok(app.txc.take())
    }
    #[cfg(not(all(feature = "txc", unix)))]
    {
        Ok(())
    }
}

/// Push the session's repeat/volume into the engine before fresh playback
/// starts. Idempotent; the engine keeps its own copy afterwards, so only the
/// *first* contact after a (re)start matters — which is exactly when these
/// are dead elsewhere: the `Playing`-handler reapply never fires because every
/// path that starts playback pre-flips `playback_started`.
fn push_transport_modes(app: &mut App) {
    let _ = app.svc.engine.repeat(app.transport.repeat);
    let _ = app.svc.engine.set_volume(vol_u16(app.transport.volume));
}

/// Kick off one radio fetch: resolve the seed on a blocking thread under a
/// deadline, and land the station (or the timeout error) on `tx`. The
/// in-flight guard and status text are the caller's job — both the fresh
/// radio key and the resume path share this exact shape.
fn spawn_radio(
    engine: Engine,
    seed: String,
    start_position_ms: u32,
    tx: flume::Sender<Result<Radio, String>>,
) {
    tokio::spawn(async move {
        let res = match tokio::time::timeout(
            Duration::from_secs(tuna_tui::yt::RADIO_TIMEOUT_SECS),
            async move {
                tokio::task::spawn_blocking(move || engine.radio_tracks(&seed))
                    .await
                    .map_err(|e| e.to_string())?
            },
        )
        .await
        {
            Ok(r) => r.map_err(|e| e.to_string()),
            Err(_) => Err("timed out (radio endpoint unresponsive)".to_string()),
        };
        let _ = tx.send(res.map(|uris| Radio {
            uris,
            start_position_ms,
        }));
    });
}

/// Resume the persisted playback source at the last track/position — the
/// faithful reboot resume (real context ⇒ real queue continuation).
fn resume_source(app: &mut App, radio_tx: &flume::Sender<Result<Radio, String>>) {
    push_transport_modes(app);
    let track = app
        .playback
        .now
        .as_ref()
        .map(|n| n.uri.clone())
        .filter(|u| !u.is_empty());
    let pos = app
        .playback
        .now
        .as_ref()
        .map(|n| n.position_ms)
        .unwrap_or(0);

    match app.transport.source.clone() {
        PlaySource::Context(ctx) => {
            if let Err(e) = app
                .svc
                .engine
                .play_context_at(ctx, track, pos, app.transport.shuffle)
            {
                app.status = format!("couldn't play: {e:#}");
            }
        }
        PlaySource::Radio(seed) => {
            // Same in-flight guard the Enter path uses: a resumed station must
            // not race a fresh radio request into the same drain.
            app.session.radio_in_flight = true;
            app.status = "resuming radio…".to_string();
            spawn_radio(app.svc.engine.clone(), seed, pos, radio_tx.clone());
        }
        PlaySource::Liked if !app.browse.library.liked.is_empty() => {
            let uris: Vec<String> = app
                .browse
                .library
                .liked
                .iter()
                .map(|i| i.uri.clone())
                .collect();
            if let Err(e) = app
                .svc
                .engine
                .play_tracks(uris, track, pos, app.transport.shuffle)
            {
                app.status = format!("couldn't play: {e:#}");
            }
        }
        _ => {
            // No known context — resume the last track followed by the saved
            // queue so playback actually continues past the first song.
            if !app.transport.queue_uris.is_empty() {
                let mut uris = Vec::with_capacity(app.transport.queue_uris.len() + 1);
                if let Some(u) = &track {
                    uris.push(u.clone());
                }
                uris.extend(app.transport.queue_uris.iter().cloned());
                if let Err(e) = app
                    .svc
                    .engine
                    .play_tracks(uris, track, pos, app.transport.shuffle)
                {
                    app.status = format!("couldn't play: {e:#}");
                }
            } else {
                match track {
                    Some(uri) => {
                        if let Err(e) = app.svc.engine.play_track_at(uri, pos) {
                            app.status = format!("couldn't play: {e:#}");
                        }
                    }
                    None => {
                        if let Err(e) = app.svc.engine.play() {
                            app.status = format!("couldn't play: {e:#}");
                        }
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
#[path = "main_tests/mod.rs"]
mod main_tests;
