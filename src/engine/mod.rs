//! The local playback engine (phase 2 of the Spotify → YouTube port).
//!
//! Replaces librespot's Connect device with a plain local player: an
//! [`Expander`] turns a track URI into a direct stream URL, ffmpeg decodes it
//! into raw PCM, rodio plays it, and the same PCM is tee'd into the shared
//! FFT bands the renderer reads. The public [`Engine`] facade and the
//! [`EngineEvent`] set keep their pre-port shape — the app layer drives this
//! exactly as it drove librespot. Events are driven by the decoder: EOF ends a
//! track, position is extrapolated from the sample counter, shuffle/repeat and
//! the mixer live locally.
//!
//! Threading: one worker thread owns the audio device, the rodio player and
//! the ffmpeg child, and answers commands off a flume channel. A watchdog
//! thread polls a shared health cell (5 s cadence) and asks the worker to
//! re-resolve + restart on a stalled or failed stream, with the same 5–120 s
//! backoff the old reconnect loop used. The engine needs no tokio runtime.

pub mod expander;
mod ffmpeg_source;

use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player};

use crate::audio::VisBands;
use crate::liblog::liblog;

pub use expander::{Expander, ResolvedTrack, YtExpander, RADIO_LIMIT};
use ffmpeg_source::FfmpegSource;

/// A normalized playback event surfaced to the rest of the app. The set is
/// identical to the librespot engine's — phase 2 changes how they are
/// produced, not their shape.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// A new track became current — carries its uri. The reactive theme
    /// trigger (the cover/theme pipeline applies on metadata arrival).
    TrackChanged {
        uri: String,
    },
    Playing {
        uri: String,
        position_ms: u32,
    },
    Paused {
        uri: String,
        position_ms: u32,
    },
    Stopped,
    /// The stream went away and playback is being rebuilt from a fresh resolve.
    Reconnecting,
    /// Playback control works again; the stream was restarted from its position.
    Reconnected,
    EndOfTrack {
        uri: String,
    },
    PositionCorrection {
        uri: String,
        position_ms: u32,
    },
}

/// Metadata the engine ships in-band for every resolved track — the only
/// metadata source since the Web API died. The app maps this onto its own
/// `TrackMeta` and feeds the cover/theme/lyrics pipeline.
pub struct EngineMeta {
    pub uri: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u32,
    pub image_url: Option<String>,
    pub image: Option<image::DynamicImage>,
    pub theme: Option<crate::theme::Theme>,
}

/// Everything the worker answers commands through.
struct Inner {
    cmds: flume::Sender<Cmd>,
    expander: Arc<dyn Expander>,
    /// The loaded play list, mirrored from the worker so the app can render its
    /// own queue without a server (`Engine::queue`). Written on every Load and
    /// Stop; the worker's `PlayerState.tracks` is the authority mid-flight.
    queue: Arc<Mutex<Vec<String>>>,
}

/// A running engine: keep it alive (dropping it tears the worker down) and
/// read `bands` for the live visualizer. Cheap to clone — every clone shares
/// the same worker — so a background task can drive it.
#[derive(Clone)]
pub struct Engine {
    pub bands: Arc<Mutex<VisBands>>,
    inner: Arc<Inner>,
}

/// Commands the facade hands the worker. `Load` carries a fully-expanded
/// queue — expansion happens in the facade so a resolve failure surfaces as
/// the caller's `Err`.
enum Cmd {
    Load {
        tracks: Vec<String>,
        start_uri: Option<String>,
        position_ms: u32,
        shuffle: bool,
    },
    Resume,
    Pause,
    Toggle,
    Next,
    Prev,
    Seek(u32),
    Volume(f32),
    Shuffle(bool),
    Repeat(bool),
    /// Append tracks to the loaded queue (the menu's "Add to Queue").
    Append(Vec<String>),
    Stop,
    /// Watchdog-initiated stream recovery (stall or decode failure).
    Recover,
}

/// The local queue: the loaded context plus a replay history for `prev`.
struct PlayerState {
    tracks: Vec<String>,
    cursor: usize,
    history: Vec<usize>,
    shuffle: bool,
    repeat: bool,
    volume: f32,
    playing: bool,
}

/// One playing (or paused) track: the ffmpeg child and the bookkeeping the
/// worker needs to drive it.
struct CurrentTrack {
    uri: String,
    /// The resolved direct URL; reused for `-ss` restarts (seek).
    url: String,
    position_ms: u32,
    /// The resolved track's known length, when the resolver knows it — lets
    /// `track_ended` tell a genuinely short song from a dropped stream.
    duration_ms: Option<u32>,
    child: Child,
    /// rodio's sound-done signal (fires when the audio thread consumed the
    /// last sample — EOF, or a shorter abort via `Player::clear`).
    done: std::sync::mpsc::Receiver<()>,
    /// Per-channel samples delivered (the playhead authority).
    frames: Arc<AtomicU64>,
    /// Shared with the source; flipped before killing the child so the old
    /// sound ends on the next callback instead of draining its backlog.
    cancelled: Arc<AtomicBool>,
}

/// The cell the watchdog polls. Only `playing` + `last_progress` matter: a
/// track that claims to play but hasn't advanced frames is a stall.
struct Health {
    playing: bool,
    last_progress: Instant,
}

impl Engine {
    /// Start a context (playlist / album / artist / track URI). When
    /// `shuffle` is set, the whole expanded context shuffles locally.
    pub fn play_context(&self, context_uri: impl Into<String>, shuffle: bool) -> Result<()> {
        let uri = context_uri.into();
        let tracks = self.inner.expander.expand(&uri).map_err(|e| anyhow!(e))?;
        self.send(Cmd::Load {
            tracks,
            start_uri: None,
            position_ms: 0,
            shuffle,
        })
    }

    /// Load a context and start at a specific track + position (context
    /// resume).
    pub fn play_context_at(
        &self,
        context_uri: String,
        track_uri: Option<String>,
        position_ms: u32,
        shuffle: bool,
    ) -> Result<()> {
        let tracks = self
            .inner
            .expander
            .expand(&context_uri)
            .map_err(|e| anyhow!(e))?;
        self.send(Cmd::Load {
            tracks,
            start_uri: track_uri,
            position_ms,
            shuffle,
        })
    }

    /// Play an explicit list of track URIs as a queue. `start_uri` picks the
    /// first track (ignored under shuffle); `shuffle` shuffles the whole list
    /// locally — so shuffling Liked Songs covers every track passed in.
    pub fn play_tracks(
        &self,
        tracks: Vec<String>,
        start_uri: Option<String>,
        start_position_ms: u32,
        shuffle: bool,
    ) -> Result<()> {
        if tracks.is_empty() {
            return Ok(());
        }
        self.send(Cmd::Load {
            tracks,
            start_uri,
            position_ms: start_position_ms,
            shuffle,
        })
    }

    /// Load a single track and start playing at `position_ms` — used to resume
    /// the last session's track when the user first hits play.
    pub fn play_track_at(&self, uri: String, position_ms: u32) -> Result<()> {
        self.send(Cmd::Load {
            tracks: vec![uri.clone()],
            start_uri: Some(uri),
            position_ms,
            shuffle: false,
        })
    }

    pub fn play(&self) -> Result<()> {
        self.send(Cmd::Resume)
    }
    pub fn pause(&self) -> Result<()> {
        self.send(Cmd::Pause)
    }
    /// Append tracks to the loaded queue — the app's "Add to Queue" menu
    /// entry. They play after the current list exhausts (`next`/EOF reach the
    /// grown end). A no-op while nothing is loaded, mirroring the old server
    /// queue's requirement that a context be playing.
    pub fn enqueue(&self, uris: Vec<String>) -> Result<()> {
        if uris.is_empty() {
            return Ok(());
        }
        self.send(Cmd::Append(uris))
    }

    /// Stop and clear the queue. (`()` to match the pre-port signature.)
    pub fn stop(&self) {
        let _ = self.send(Cmd::Stop);
    }
    pub fn toggle(&self) -> Result<()> {
        self.send(Cmd::Toggle)
    }
    pub fn next(&self) -> Result<()> {
        self.send(Cmd::Next)
    }
    pub fn prev(&self) -> Result<()> {
        self.send(Cmd::Prev)
    }
    pub fn shuffle(&self, on: bool) -> Result<()> {
        self.send(Cmd::Shuffle(on))
    }
    pub fn repeat(&self, on: bool) -> Result<()> {
        self.send(Cmd::Repeat(on))
    }
    /// Set volume in the engine's `0..=65535` range (mirrors the pre-port
    /// mixer contract; applied linearly).
    pub fn set_volume(&self, vol: u16) -> Result<()> {
        self.send(Cmd::Volume(vol as f32 / 65_535.0))
    }
    /// Seek to an absolute position in the current track (restarts the
    /// decoder at the new offset).
    pub fn seek(&self, position_ms: u32) -> Result<()> {
        self.send(Cmd::Seek(position_ms))
    }
    /// The radio station for a seed track (seed followed by similar uris).
    pub fn radio_tracks(&self, seed: &str) -> Result<Vec<String>, String> {
        self.inner.expander.radio(seed)
    }

    /// The loaded play list, in play order (post-shuffle). Empty when nothing
    /// is loaded. A local mirror for the app's Queue view — the old provider
    /// queue (`/me/player/queue`) died with the Spotify port.
    pub fn queue(&self) -> Vec<String> {
        self.inner
            .queue
            .lock()
            .map(|q| q.clone())
            .unwrap_or_default()
    }

    fn send(&self, cmd: Cmd) -> Result<()> {
        self.inner
            .cmds
            .send(cmd)
            .map_err(|_| anyhow!("engine not running"))
    }
}

/// How long the watchdog waits between checks. A lock read while healthy;
/// only an actual recovery costs anything.
const HEALTH_CHECK: Duration = Duration::from_secs(5);
/// Audio must advance at least this often while "playing" or the stream is
/// considered stalled (ffmpeg hung, network dead) and gets rebuilt.
const STALL_AFTER: Duration = Duration::from_secs(15);
/// An EOF with less than this much delivered audio is a dropped stream, not a
/// finished track (see [`Worker::track_ended`]): on this box googlevideo
/// connections die a few hundred ms in and ffmpeg exits 0, indistinguishable
/// from a natural end by exit code alone. 5 s is well under any real song.
const MIN_EOF_POSITION_MS: u32 = 5_000;
/// How many consecutive short-EOF drops on the same track before it is given
/// up on (skipped or, at the queue tail with repeat off, stopped cleanly)
/// instead of rebuilding forever.
const MAX_EOF_DROPS: u32 = 8;
/// First and last wait between failed recovery attempts, so an offline spell
/// doesn't hammer the resolver every five seconds until dawn.
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(120);
/// How many recovery attempts before the track is given up on.
const RECOVER_ATTEMPTS: u32 = 8;
/// Worker loop wakeup: drains commands, watches for EOF, emits position.
const TICK: Duration = Duration::from_millis(100);
/// How often the worker emits a [`EngineEvent::PositionCorrection`] while
/// playing, to trim the app-side extrapolated playhead.
const POSITION_EVERY: Duration = Duration::from_secs(1);

fn next_backoff(current: Duration) -> Duration {
    (current * 2).min(RETRY_MAX)
}

/// Open the output device and build the player + the per-track sound queue.
/// Runs on the caller's thread so a failure ("no audio device") surfaces from
/// `run`, not a worker.
///
/// The queue pair is the engine's "append" surface: the output side is appended
/// to the player once (it plays silence when empty), and each track is a
/// `append_with_signal`d sound on the input side whose exposed receiver is the
/// EOF signal. `Player` keeps its own queue private, so the pair sits outside
/// it — volume/pause/play still go through the `Player`.
fn open_output() -> Result<(
    MixerDeviceSink,
    Player,
    Arc<rodio::queue::SourcesQueueInput>,
)> {
    // Device faults (cpal's `BufferUnderrun` on an ALSA/PipeWire xrun) go to
    // the tuna-tui log instead of rodio's default raw `eprintln!` storming the
    // terminal beside the TUI. The closure captures nothing, so the builder
    // stays `Clone` for `open_sink_or_fallback`'s config fallback.
    let mut sink = DeviceSinkBuilder::from_default_device()
        .map_err(|e| anyhow!("audio device: {e}"))?
        .with_error_callback(|err| liblog(format!("audio stream error: {err}")))
        .open_sink_or_fallback()
        .context("open audio device")?;
    sink.log_on_drop(false);
    let player = Player::connect_new(sink.mixer());
    let (queue_in, queue_out) = rodio::queue::queue(true);
    player.append(queue_out);
    Ok((sink, player, queue_in))
}

/// Start the engine. Synchronous (it needs no runtime); the worker + watchdog
/// threads are spawned here. `expander` resolves uris into streams; `events`
/// receives the EngineEvent stream; `meta_tx` receives in-band metadata for
/// yt: tracks.
pub fn run(
    events: flume::Sender<EngineEvent>,
    meta_tx: flume::Sender<EngineMeta>,
    initial_volume_pct: u8,
    expander: Arc<dyn Expander>,
) -> Result<Engine> {
    let bands = VisBands::shared();
    let (sink, player, queue) = open_output()?;
    let (cmds_tx, cmds_rx) = flume::unbounded::<Cmd>();
    let health = Arc::new(Mutex::new(Health {
        playing: false,
        last_progress: Instant::now(),
    }));

    let queue_snapshot = Arc::new(Mutex::new(Vec::new()));
    let worker = Worker {
        sink,
        player,
        queue,
        bands: Arc::clone(&bands),
        events,
        meta_tx,
        cmds: cmds_rx,
        expander: Arc::clone(&expander),
        queue_snapshot: Arc::clone(&queue_snapshot),
        state: PlayerState {
            tracks: Vec::new(),
            cursor: 0,
            history: Vec::new(),
            shuffle: false,
            repeat: false,
            volume: initial_volume_pct.clamp(0, 100) as f32 / 100.0,
            playing: false,
        },
        current: None,
        health: Arc::clone(&health),
        drop_streak: 0,
        last_seen_frames: 0,
        last_correction: Instant::now(),
        client: http_client(),
        pending: None,
        recovery: None,
    };
    std::thread::Builder::new()
        .name("tuna-engine".to_string())
        .spawn(move || worker.run())?;
    spawn_watchdog(Arc::clone(&health), cmds_tx.clone());

    Ok(Engine {
        bands,
        inner: Arc::new(Inner {
            cmds: cmds_tx,
            expander,
            queue: queue_snapshot,
        }),
    })
}

/// A blocking client for cover fetches (the api layer's helper lives bin-side
/// and dies with phase 3; httpcache keeps the disk cache shared).
fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default()
}

/// Poll for a stuck stream and ask the worker to rebuild it.
///
/// Holds only a *weak* sender: when the app drops its last [`Engine`], the
/// worker's channel disconnects, the loop's `upgrade()` returns `None`, and
/// this thread leaves — letting the worker see `Disconnected` and run its
/// teardown instead of lingering anonymous forever (the strong sender held by
/// the watchdog used to make that teardown unreachable).
fn spawn_watchdog(health: Arc<Mutex<Health>>, cmds: flume::Sender<Cmd>) {
    let weak = cmds.downgrade();
    let _ = std::thread::Builder::new()
        .name("tuna-watchdog".to_string())
        .spawn(move || loop {
            std::thread::sleep(HEALTH_CHECK);
            // `upgrade()` is also the liveness probe: none left → retire.
            let Some(cmds) = weak.upgrade() else {
                return;
            };
            let h = match health.lock() {
                Ok(h) => h,
                Err(p) => p.into_inner(),
            };
            if h.playing && h.last_progress.elapsed() > STALL_AFTER {
                drop(h);
                // The worker clears `playing` while it rebuilds, so this can
                // never stack recoveries; it re-arms at the next poll.
                let _ = cmds.send(Cmd::Recover);
            }
        });
}

struct Worker {
    /// Held alive for the worker's whole life: dropping it stops the device.
    #[allow(dead_code)] // the guard's whole job is to be held, never read
    sink: MixerDeviceSink,
    player: Player,
    /// The per-track sound queue: tracks are appended here, EOF signals come
    /// back from its receivers.
    queue: Arc<rodio::queue::SourcesQueueInput>,
    bands: Arc<Mutex<VisBands>>,
    events: flume::Sender<EngineEvent>,
    meta_tx: flume::Sender<EngineMeta>,
    cmds: flume::Receiver<Cmd>,
    expander: Arc<dyn Expander>,
    /// The public mirror of the loaded list (`Engine::queue`).
    queue_snapshot: Arc<Mutex<Vec<String>>>,
    state: PlayerState,
    current: Option<CurrentTrack>,
    health: Arc<Mutex<Health>>,
    /// Consecutive short-EOF drops on the current track (mirrors
    /// [`MAX_EOF_DROPS`]); reset on a natural end or a new track.
    drop_streak: u32,
    /// The last frame count seen (stall detection needs deltas).
    last_seen_frames: u64,
    last_correction: Instant,
    client: reqwest::blocking::Client,
    /// A user command that pre-empted a recovery retry-sleep; handled before
    /// anything queued behind it.
    pending: Option<Cmd>,
    /// The track a recovery is rebuilding, while `current` is nowhere. The
    /// watchdog and a pre-empted resume use it to re-enter the rebuild loop
    /// instead of losing the track.
    recovery: Option<(String, u32)>,
}

impl Worker {
    fn run(mut self) {
        self.player.set_volume(self.state.volume);
        loop {
            match self.cmds.recv_timeout(TICK) {
                Ok(cmd) => self.handle(cmd),
                Err(flume::RecvTimeoutError::Timeout) => self.tick(),
                Err(flume::RecvTimeoutError::Disconnected) => break,
            }
            if let Some(pre) = self.pending.take() {
                self.handle(pre);
                // The pre-empting command ran — if the recovery it interrupted
                // is still owed and nothing else took the stage, re-enter it.
                if let Some((uri, pos)) = self.recovery.take() {
                    if self.current.is_none() {
                        self.recover_into(uri, pos);
                    }
                }
            }
        }
        self.teardown();
    }

    /// The per-tick (100 ms) bookkeeping: EOF, health, position.
    fn tick(&mut self) {
        // Collect the EOF flag under the borrow, then act on it after the
        // borrow of `current` is released (`track_ended` needs `&mut self`).
        let mut ended = false;
        if let Some(cur) = self.current.as_mut() {
            let f = cur.frames.load(Ordering::Relaxed);
            if f != self.last_seen_frames {
                self.last_seen_frames = f;
                if let Ok(mut h) = self.health.lock() {
                    h.last_progress = Instant::now();
                }
            }
            // Track end: rodio fired the sound-done signal.
            ended = cur.done.try_recv().is_ok();
            if !ended && self.state.playing && self.last_correction.elapsed() >= POSITION_EVERY {
                // Derive the position from `cur` directly (no `&self` borrow
                // while `current` is mutably borrowed).
                let frames = cur.frames.load(Ordering::Relaxed);
                let pos = frames_to_position(cur.position_ms, frames);
                let uri = cur.uri.clone();
                let _ = self.events.send(EngineEvent::PositionCorrection {
                    uri,
                    position_ms: pos,
                });
                self.last_correction = Instant::now();
            }
        }
        if ended {
            self.track_ended();
        }
    }

    fn set_health(&mut self, playing: bool) {
        if let Ok(mut h) = self.health.lock() {
            h.playing = playing;
            h.last_progress = Instant::now();
        }
    }

    fn set_active(&self, active: bool) {
        if let Ok(mut b) = self.bands.lock() {
            b.is_active = active;
        }
    }

    fn reset_bands(&self) {
        if let Ok(mut b) = self.bands.lock() {
            b.values.fill(0.0);
            b.peak_envelope = 1e-6;
            b.updated_at = Instant::now();
        }
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Load {
                tracks,
                start_uri,
                position_ms,
                shuffle,
            } => {
                self.shutdown_current();
                // A fresh context supersedes any in-flight recovery.
                self.recovery = None;
                let start = start_uri
                    .as_deref()
                    .and_then(|u| tracks.iter().position(|t| t == u))
                    .unwrap_or(0);
                if let Ok(mut q) = self.queue_snapshot.lock() {
                    *q = tracks.clone();
                }
                self.state = PlayerState {
                    // `start` is the cursor: advance picks up after the first
                    // track, and history grows from it — not from 0.
                    tracks,
                    cursor: start,
                    history: Vec::new(),
                    shuffle,
                    repeat: self.state.repeat,
                    volume: self.state.volume,
                    playing: true,
                };
                self.start_track_at(start, position_ms);
            }
            Cmd::Resume => {
                if !self.state.playing && self.current.is_some() {
                    self.player.play();
                    self.state.playing = true;
                    self.set_health(true);
                    self.set_active(true);
                    let (uri, pos) = self.current_ident();
                    let _ = self.events.send(EngineEvent::Playing {
                        uri,
                        position_ms: pos,
                    });
                }
            }
            Cmd::Pause => {
                if self.state.playing && self.current.is_some() {
                    self.player.pause();
                    self.state.playing = false;
                    self.set_health(false);
                    self.set_active(false);
                    let (uri, pos) = self.current_ident();
                    let _ = self.events.send(EngineEvent::Paused {
                        uri,
                        position_ms: pos,
                    });
                }
            }
            Cmd::Toggle => {
                if self.state.playing {
                    self.handle(Cmd::Pause);
                } else {
                    self.handle(Cmd::Resume);
                }
            }
            Cmd::Next => self.advance(),
            Cmd::Prev => {
                let pos = self.current_pos();
                if self.current.is_some() && pos > 5_000 {
                    self.seek_now(0);
                } else if let Some(prev) = self.state.history.pop() {
                    self.start_track_at(prev, 0);
                } else if self.current.is_some() {
                    self.seek_now(0);
                }
            }
            Cmd::Seek(pos) => self.seek_now(pos),
            Cmd::Volume(v) => {
                self.state.volume = v;
                self.player.set_volume(v);
            }
            Cmd::Shuffle(on) => self.state.shuffle = on,
            Cmd::Repeat(on) => self.state.repeat = on,
            Cmd::Append(uris) => {
                if let Ok(mut q) = self.queue_snapshot.lock() {
                    q.extend(uris.iter().cloned());
                }
                self.state.tracks.extend(uris);
            }
            Cmd::Stop => {
                self.shutdown_current();
                self.recovery = None;
                if let Ok(mut q) = self.queue_snapshot.lock() {
                    q.clear();
                }
                self.state.tracks.clear();
                self.state.history.clear();
                self.state.cursor = 0;
                self.state.playing = false;
                self.set_health(false);
                self.set_active(false);
                self.reset_bands();
                let _ = self.events.send(EngineEvent::Stopped);
            }
            Cmd::Recover => self.recover(),
        }
    }

    /// The canonical identity of the current track, for events.
    fn current_ident(&self) -> (String, u32) {
        match &self.current {
            Some(c) => (c.uri.clone(), self.position_of(c)),
            None => (String::new(), 0),
        }
    }

    fn position_of(&self, cur: &CurrentTrack) -> u32 {
        let frames = cur.frames.load(Ordering::Relaxed);
        frames_to_position(cur.position_ms, frames)
    }

    fn current_pos(&self) -> u32 {
        match &self.current {
            Some(c) => self.position_of(c),
            None => 0,
        }
    }

    /// Move to the next track in the (possibly shuffled) queue; `None` when
    /// the queue is exhausted and repeat is off.
    fn advance_index(&mut self) -> Option<usize> {
        let n = self.state.tracks.len();
        if n == 0 {
            return None;
        }
        if self.state.shuffle && n > 1 {
            use rand::Rng as _;
            let other: Vec<usize> = (0..n).filter(|&i| i != self.state.cursor).collect();
            let pick = other[rand::rng().random_range(0..other.len())];
            self.state.history.push(self.state.cursor);
            self.state.cursor = pick;
            return Some(pick);
        }
        if self.state.cursor + 1 < n {
            self.state.history.push(self.state.cursor);
            self.state.cursor += 1;
            return Some(self.state.cursor);
        }
        if self.state.repeat {
            self.state.history.push(self.state.cursor);
            self.state.cursor = 0;
            return Some(0);
        }
        None
    }

    /// `next` (or the natural end of a finished track) — the shared body.
    fn advance(&mut self) {
        if self.current.is_none() && self.state.tracks.is_empty() {
            return;
        }
        match self.advance_index() {
            Some(idx) => self.start_track_at(idx, 0),
            None => {
                // Queue over, repeat off: stop cleanly, like the old endpoint.
                let uri = self.current_ident().0;
                self.shutdown_current();
                self.state.playing = false;
                self.set_health(false);
                self.set_active(false);
                self.reset_bands();
                if !uri.is_empty() {
                    let _ = self.events.send(EngineEvent::EndOfTrack { uri });
                }
                let _ = self.events.send(EngineEvent::Stopped);
            }
        }
    }

    /// The natural end of the current track (EOF): emit EndOfTrack and move
    /// on; if the process died instead of ending, rebuild the stream.
    ///
    /// A dropped stream is *not* an end of track: YouTube's transport (and
    /// this box's Wi-Fi, verified 2026-08-16) closes the connection mid-song,
    /// ffmpeg then exits cleanly (code 0) and the pipe EOFs with only seconds
    /// of audio delivered. Without this check the engine would treat that as
    /// a finished track and advance/stop. Anything that "ended" in under
    /// [`MIN_EOF_POSITION_MS`] of delivered playhead is treated as a failed
    /// stream and rebuilt — except when the *track itself* is that short
    /// (`duration_ms` says its real end was reached), which is a genuine EOF.
    ///
    /// Rebuilds are bounded by [`MAX_EOF_DROPS`]: each drop on the same track
    /// counts up and the track is given up on (skipped/stopped) once the
    /// streak passes, so a persistently-dead stream can't churn forever.
    fn track_ended(&mut self) {
        let Some(mut cur) = self.current.take() else {
            return;
        };
        let uri = cur.uri.clone();
        let pos = self.position_of(&cur);
        let failed = cur
            .child
            .try_wait()
            .ok()
            .flatten()
            .is_some_and(|s| s.code() != Some(0));
        let short_track = cur
            .duration_ms
            .is_some_and(|d| pos.saturating_add(3_000) >= d);
        let dropped = !failed && pos < MIN_EOF_POSITION_MS && !short_track;
        if failed || dropped {
            let _ = cur.child.kill();
            let _ = cur.child.wait();
            self.drop_streak += 1;
            if self.drop_streak >= MAX_EOF_DROPS {
                liblog(format!(
                    "engine: giving up on {uri} after {MAX_EOF_DROPS} consecutive failed EOFs"
                ));
                self.give_up_on(uri);
                return;
            }
            if dropped {
                liblog(format!(
                    "engine: stream dropped for {uri} at {pos}ms (<{MIN_EOF_POSITION_MS}ms); rebuilding"
                ));
            } else {
                liblog(format!("engine: decoder died for {uri}; rebuilding stream"));
            }
            self.recover_into(uri, pos);
            return;
        }
        self.drop_streak = 0;
        drop(cur);
        let _ = self.events.send(EngineEvent::EndOfTrack { uri });
        self.advance();
    }

    /// The track is given up on after too many consecutive failures: remove it
    /// from the queue (keeping the queue view mirror in sync) and play its
    /// successor — or stop cleanly when the queue is over and repeat is off,
    /// mirroring `advance()`'s queue-exhausted behavior.
    fn give_up_on(&mut self, uri: String) {
        self.recovery = None;
        self.drop_streak = 0;
        let dead = self.state.cursor;
        if dead < self.state.tracks.len() && self.state.tracks[dead] == uri {
            self.state.tracks.remove(dead);
        } else {
            self.state.tracks.retain(|t| *t != uri);
        }
        // History indices shift past the removed slot.
        self.state.history.retain(|&h| h != dead);
        for h in &mut self.state.history {
            if *h > dead {
                *h -= 1;
            }
        }
        *self.queue_snapshot.lock().unwrap() = self.state.tracks.clone();
        liblog(format!(
            "engine: giving up on {uri} after {MAX_EOF_DROPS} consecutive failed EOFs"
        ));
        if self.state.tracks.is_empty() {
            self.give_up_stop(uri);
        } else if dead < self.state.tracks.len() {
            self.start_track_at(dead, 0);
        } else if self.state.repeat {
            self.start_track_at(0, 0);
        } else {
            self.give_up_stop(uri);
        }
    }

    /// A watchdog stall or a failing decoder: re-resolve and restart from the
    /// current position, with the old 5–120 s backoff. Gives up (skips the
    /// track) after [`RECOVER_ATTEMPTS`].
    fn recover(&mut self) {
        let (uri, pos) = self.current_ident();
        if uri.is_empty() {
            // A recovery in flight owns the uri even while `current` is gone.
            if let Some((uri, pos)) = self.recovery.clone() {
                self.recover_into(uri, pos);
            }
            return;
        }
        self.recover_into(uri, pos);
    }

    fn recover_into(&mut self, uri: String, pos: u32) {
        self.shutdown_current();
        self.set_health(false); // watchdog off while we rebuild
                                // A paused player stays paused: the stalled stream is rebuilt into
                                // the state it left behind, never force-played.
        let play = self.state.playing;
        self.recovery = Some((uri.clone(), pos));

        let mut backoff = RETRY_MIN;
        for attempt in 0..RECOVER_ATTEMPTS {
            if attempt > 0 {
                let _ = self.events.send(EngineEvent::Reconnecting);
            }
            match self.build_stream(&uri, pos, play) {
                Ok(()) => {
                    if attempt > 0 {
                        let _ = self.events.send(EngineEvent::Reconnected);
                    }
                    self.recovery = None;
                    return;
                }
                Err(e) => {
                    liblog(format!("engine: recover {uri} attempt {attempt}: {e}"));
                    if attempt + 1 >= RECOVER_ATTEMPTS {
                        break;
                    }
                    if let Some(pre) = self.interruptible_sleep(backoff) {
                        // A user command pre-empted the wait; it is dispatched
                        // by `run` before anything queued behind it, which then
                        // re-enters this loop via `recovery`.
                        self.pending = Some(pre);
                        return;
                    }
                    backoff = next_backoff(backoff);
                }
            }
        }
        self.recovery = None;
        self.give_up_on(uri);
    }

    /// The queue is empty after a give-up: stop cleanly (EndOfTrack + Stopped)
    /// instead of leaving `playing=true` with nothing current — a state no
    /// command could escape.
    fn give_up_stop(&mut self, uri: String) {
        self.state.playing = false;
        self.set_health(false);
        self.set_active(false);
        self.reset_bands();
        let _ = self.events.send(EngineEvent::EndOfTrack { uri });
        let _ = self.events.send(EngineEvent::Stopped);
    }

    fn interruptible_sleep(&self, dur: Duration) -> Option<Cmd> {
        let deadline = Instant::now() + dur;
        while Instant::now() < deadline {
            if let Ok(c) = self.cmds.try_recv() {
                return Some(c);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }

    /// Stop the current child. Flipping the source's cancel flag first makes it
    /// end on the next audio callback, so the old track's buffered PCM is never
    /// heard after a swap; the stale done-signal rides out with the track object.
    fn shutdown_current(&mut self) {
        if let Some(mut cur) = self.current.take() {
            cur.cancelled.store(true, Ordering::Relaxed);
            let _ = cur.child.kill();
            let _ = cur.child.wait();
        }
    }

    /// Begin resolving, decoding and playing `tracks[idx]`.
    fn start_track_at(&mut self, idx: usize, pos: u32) {
        let Some(uri) = self.state.tracks.get(idx).cloned() else {
            return;
        };
        self.shutdown_current();
        self.state.cursor = idx;
        self.state.playing = true;
        self.set_health(false);
        self.drop_streak = 0; // a fresh track starts with a clean slate
        let _ = self
            .events
            .send(EngineEvent::TrackChanged { uri: uri.clone() });

        if let Err(e) = self.build_stream(&uri, pos, true) {
            liblog(format!("engine: start {uri} failed: {e}"));
            self.recover_into(uri, pos);
        }
    }

    /// Resolve `uri`, spawn ffmpeg, append the source and announce Playing — or,
    /// when `play` is false, announce Paused and keep the mixer suspended (a
    /// recovery rebuilding a track the user had paused must not force-play it).
    fn build_stream(&mut self, uri: &str, pos: u32, play: bool) -> Result<()> {
        let resolved = self.expander.resolve(uri).map_err(|e| anyhow!(e))?;
        let url = resolved.url.clone();
        // `-ss` seeks on whole seconds; anchor the playhead at the truncated
        // position so the extrapolation doesn't run ahead of the decoder.
        let pos = pos / 1000 * 1000;
        let (child, stdout) = spawn_ffmpeg(&url, pos)?;
        let frames = Arc::new(AtomicU64::new(0));
        let cancelled = Arc::new(AtomicBool::new(false));
        let source = FfmpegSource::new(
            stdout,
            Arc::clone(&frames),
            Arc::clone(&self.bands),
            Arc::clone(&cancelled),
        );
        let done = self.queue.append_with_signal(source);
        if play {
            self.player.play();
        } else {
            self.player.pause();
        }
        self.current = Some(CurrentTrack {
            uri: uri.to_string(),
            url,
            position_ms: pos,
            duration_ms: resolved.duration_ms,
            child,
            done,
            frames,
            cancelled,
        });
        self.last_seen_frames = 0;
        self.state.playing = play;
        self.set_health(play);
        self.set_active(play);
        self.last_correction = Instant::now();
        let _ = self.events.send(if play {
            EngineEvent::Playing {
                uri: uri.to_string(),
                position_ms: pos,
            }
        } else {
            EngineEvent::Paused {
                uri: uri.to_string(),
                position_ms: pos,
            }
        });
        // In-band metadata for every resolved track (the app has no other metadata
        // source since the Web API died). Sent on a detached thread — the cover
        // fetch + theme derive must not block the worker's state machine while
        // a slow request stalls.
        let metatx = self.meta_tx.clone();
        let client = self.client.clone();
        let u = uri.to_string();
        let info = resolved.clone();
        if std::thread::Builder::new()
            .name("tuna-meta".into())
            .spawn(move || {
                let _ = metatx.send(engine_meta(&u, &info, &client));
            })
            .is_err()
        {
            liblog("engine: failed to spawn meta thread");
        }
        Ok(())
    }

    /// Seek: restart the decoder at `pos` on the current stream URL.
    fn seek_now(&mut self, pos: u32) {
        let Some(mut cur) = self.current.take() else {
            return;
        };
        let url = cur.url.clone();
        let uri = cur.uri.clone();
        cur.cancelled.store(true, Ordering::Relaxed);
        let _ = cur.child.kill();
        let _ = cur.child.wait();
        // Same whole-second anchor as `build_stream` uses.
        let pos = pos / 1000 * 1000;
        match spawn_ffmpeg(&url, pos) {
            Ok((child, stdout)) => {
                let frames = Arc::new(AtomicU64::new(0));
                let cancelled = Arc::new(AtomicBool::new(false));
                let source = FfmpegSource::new(
                    stdout,
                    Arc::clone(&frames),
                    Arc::clone(&self.bands),
                    Arc::clone(&cancelled),
                );
                let done = self.queue.append_with_signal(source);
                self.current = Some(CurrentTrack {
                    uri: uri.clone(),
                    url,
                    position_ms: pos,
                    duration_ms: cur.duration_ms,
                    child,
                    done,
                    frames,
                    cancelled,
                });
                self.drop_streak = 0;
                self.last_seen_frames = 0;
                // Keep the paused state a seek performed while paused.
                self.set_health(self.state.playing);
                self.set_active(self.state.playing);
                self.last_correction = Instant::now();
                if self.state.playing {
                    self.player.play();
                }
                let _ = self.events.send(EngineEvent::PositionCorrection {
                    uri,
                    position_ms: pos,
                });
            }
            Err(e) => {
                liblog(format!("engine: seek failed: {e}"));
                self.set_health(false);
                self.recover_into(uri, pos);
            }
        }
    }

    fn teardown(&mut self) {
        if let Some(mut cur) = self.current.take() {
            cur.cancelled.store(true, Ordering::Relaxed);
            let _ = cur.child.kill();
            let _ = cur.child.wait();
        }
        self.set_active(false);
        self.set_health(false);
    }
}

/// Spawn ffmpeg decoding `url` into raw stereo s16 at 44.1 kHz on stdout,
/// seeking to `pos` first (input seek — fast). Returns the child (for
/// kill/reap) plus its stdout, which is where the PCM comes from.
fn spawn_ffmpeg(url: &str, pos: u32) -> Result<(Child, std::process::ChildStdout)> {
    let bin = crate::config::get().ffmpeg_path.clone();
    let mut cmd = Command::new(&bin);
    cmd.args(["-v", "error", "-hide_banner", "-nostdin"]);
    if pos > 0 {
        cmd.arg("-ss").arg(format!("{}", pos / 1000));
    }
    cmd.arg("-i")
        .arg(url)
        .args([
            "-map", "0:a:0", "-vn", "-f", "s16le", "-ac", "2", "-ar", "44100", "-",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .env("FFMPEG_NO_AUTO_CPU_FLAGS", "1");
    let mut child = cmd.spawn().context("spawn ffmpeg")?;
    // Catch an immediate self-exit (bad URL, missing lib) — the pipe would
    // otherwise read EOF immediately and look like a two-sample track.
    if let Some(status) = child.try_wait().ok().flatten() {
        return Err(anyhow!("ffmpeg exited immediately: {status}"));
    }
    let stdout = child.stdout.take().expect("ffmpeg stdout piped");
    Ok((child, stdout))
}

/// Position derivation, pure for testing: `start_ms + frames / 44.1`.
/// Saturating: hostile frame counts clamp to the position ceiling instead of
/// wrapping (u64::MAX × 1000 overflows a u64).
fn frames_to_position(start_ms: u32, frames: u64) -> u32 {
    (start_ms as u64)
        .saturating_add(frames.saturating_mul(1000) / 44_100)
        .min(u32::MAX as u64) as u32
}

/// Build in-band metadata for a yt: track, fetching its cover + theme the
/// same way the api layer did (httpcache-keyed, 24 h TTL).
fn engine_meta(uri: &str, r: &ResolvedTrack, client: &reqwest::blocking::Client) -> EngineMeta {
    let mut image = None;
    let mut theme = None;
    if let Some(url) = &r.thumbnail {
        if let Some(bytes) = fetch_cover(client, url) {
            if let Ok(img) = image::load_from_memory(&bytes) {
                theme = Some(crate::reactive::derive_theme(&img, "album ✦"));
                image = Some(img);
            }
        }
    }
    EngineMeta {
        uri: uri.to_string(),
        title: r.title.clone(),
        artist: r.artist.clone(),
        album: r.album.clone().unwrap_or_default(),
        duration_ms: r.duration_ms.unwrap_or(0),
        image_url: r.thumbnail.clone(),
        image,
        theme,
    }
}

/// Cover bytes, from disk when they've been seen before; error pages are
/// never cached (identical policy to the api-layer fetch_cover).
fn fetch_cover(client: &reqwest::blocking::Client, url: &str) -> Option<Vec<u8>> {
    if let Some(bytes) = crate::httpcache::get_bytes(url) {
        return Some(bytes);
    }
    let resp = client.get(url).send().ok()?;
    if !resp.status().is_success() {
        liblog(format!("cover: {url} -> HTTP {}", resp.status().as_u16()));
        return None;
    }
    let bytes = resp.bytes().ok()?.to_vec();
    crate::httpcache::put_bytes(url, &bytes);
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failing_recovery_backs_off_up_to_the_cap() {
        assert!(next_backoff(RETRY_MIN) > RETRY_MIN);
        let mut wait = RETRY_MIN;
        for _ in 0..10 {
            wait = next_backoff(wait);
            assert!(wait <= RETRY_MAX, "{wait:?} is past the cap");
        }
        // An offline night must settle at the cap rather than grow without
        // bound — the watchdog has to still be trying when the network returns.
        assert_eq!(wait, RETRY_MAX);
    }

    #[test]
    fn volume_maps_linear_units() {
        // The pre-port mixer's linear 0..=65535 scale.
        assert_eq!(65_535_f32 / 65_535.0, 1.0);
        assert_eq!(32_767_f32 / 65_535.0, 0.49999237);
        assert_eq!(0_f32 / 65_535.0, 0.0);
    }

    #[test]
    fn position_derivation_adds_frames() {
        // 44_100 frames = one second of audio.
        assert_eq!(frames_to_position(5_000, 44_100), 6_000);
        assert_eq!(frames_to_position(0, 0), 0);
        // Huge counts clamp rather than wrap.
        assert_eq!(frames_to_position(0, u64::MAX), u32::MAX);
    }

    #[test]
    fn ffmpeg_cmd_seeks_whole_seconds_only() {
        // The engine seeks on whole seconds: `-ss` carries the truncated
        // position so the playhead anchor matches what the decoder did.
        let bin = crate::config::get().ffmpeg_path.clone();
        let mut cmd = Command::new(&bin);
        cmd.arg("-ss").arg("5");
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let i = args.iter().position(|a| a == "-ss").expect("-ss present");
        assert_eq!(args[i + 1], "5");
    }

    /// Oracle for the playback chain on the ACTUAL machine: does a known 1 s
    /// buffer, appended through the engine's exact queue pattern, reach the
    /// device and finish? The done receiver fires only after the mixer truly
    /// consumed the sound. A timeout here = the rodio/cpal layer itself is
    /// broken on this box (device config, mixer wiring), independently of
    /// ffmpeg/yt-dlp. Needs an audio device; `#[ignore]`d for CI.
    #[test]
    #[ignore]
    fn device_pump_plays_a_known_buffer_to_eof() {
        let (sink, player, queue_in) = open_output().expect("open device");
        // The sink MUST stay alive: rodio disposes the OS stream when the
        // DeviceSink drops — dropping it here would end playback before the
        // buffer is even appended.
        let _sink = sink;
        player.play();
        // Silent by policy (headphones): volume 0 still exercises the full
        // data path — mixer, device callback, done-signal — just inaudibly.
        player.set_volume(0.0);
        let tone: Vec<f32> = (0..44_100usize * 2)
            .map(|i| {
                let v = (i / 2) as f32 / 44_100.0;
                (2.0 * std::f32::consts::PI * 440.0 * v).sin() * 0.2
            })
            .collect();
        let buf = rodio::buffer::SamplesBuffer::new(
            rodio::math::nz!(2u16),
            rodio::math::nz!(44_100u32),
            tone,
        );
        let done = queue_in.append_with_signal(buf);
        match done.recv_timeout(Duration::from_secs(4)) {
            Ok(()) => {}
            Err(e) => panic!("device never consumed the 1s buffer: {e:?}"),
        }
    }

    /// Oracle #2: the FULL engine source path — FftSource over a real ffmpeg
    /// child reading a LOCAL wav (no network, no yt-dlp). The playhead counter
    /// must advance: this splits 'ffmpeg/network stall' from 'source/channel
    /// bug'. Needs an audio device; `#[ignore]`d for CI.
    #[test]
    #[ignore]
    fn fftsource_over_local_ffmpeg_advances_frames() {
        // Build a 2s tone wav in the target dir.
        let wav = std::env::temp_dir().join("tuna-tui-oracle-tone.wav");
        let status = std::process::Command::new(crate::config::get().ffmpeg_path.as_str())
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-ac",
                "2",
                "-ar",
                "44100",
                wav.to_str().unwrap_or("/tmp/tone.wav"),
            ])
            .status()
            .expect("ffmpeg gen");
        assert!(status.success(), "tone generation failed");

        let (sink, player, queue_in) = open_output().expect("open device");
        let _sink = sink;
        player.play();
        // Silent by policy (headphones): volume 0 keeps the source path
        // exercised end to end without reaching the listener's ears.
        player.set_volume(0.0);
        let (mut child, stdout) = spawn_ffmpeg(wav.to_str().unwrap(), 0).expect("spawn ffmpeg");
        let frames = Arc::new(AtomicU64::new(0));
        let cancelled = Arc::new(AtomicBool::new(false));
        let source = FfmpegSource::new(stdout, Arc::clone(&frames), VisBands::shared(), cancelled);
        queue_in.append_with_signal(source);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if frames.load(Ordering::Relaxed) > 1000 {
                let _ = child.kill();
                let _ = child.wait();
                return; // frames advanced: the source path works
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        panic!(
            "frames stayed ~0 with a local file: {}",
            frames.load(Ordering::Relaxed)
        );
    }

    /// Regression: the FFT tee must keep feeding while unplayed audio is
    /// still buffered, even when the pump outruns the playhead (instant for
    /// local files, bursty for network). No audio device involved: the
    /// source is driven by hand at CPU speed, which is exactly the
    /// delivery-outruns-playback condition that froze the spectrum the
    /// moment music became audible (2026-08-16). With the greedy fold, the
    /// channel empties in the first few calls and the bands go stale while
    /// `pending` still holds seconds of audio; the feed must survive to the
    /// end of the run.
    #[test]
    fn visualizer_feed_survives_a_pump_that_outruns_playback() {
        let wav = std::env::temp_dir().join("tuna-tui-oracle-tone-2s.wav");
        let gen = std::process::Command::new(crate::config::get().ffmpeg_path.as_str())
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-ac",
                "2",
                "-ar",
                "44100",
                wav.to_str().unwrap_or("/tmp/tone2s.wav"),
            ])
            .status()
            .expect("ffmpeg gen");
        assert!(gen.success(), "tone generation failed");

        let mut child = std::process::Command::new(crate::config::get().ffmpeg_path.as_str())
            .args([
                "-v",
                "error",
                "-nostdin",
                "-i",
                wav.to_str().unwrap_or("/tmp/tone2s.wav"),
                "-f",
                "s16le",
                "-ac",
                "2",
                "-ar",
                "44100",
                "-",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn decoder");
        let stdout = child.stdout.take().expect("piped stdout");
        let bands = VisBands::shared();
        let mut source = FfmpegSource::new(
            stdout,
            Arc::new(AtomicU64::new(0)),
            Arc::clone(&bands),
            Arc::new(AtomicBool::new(false)),
        );
        // ~3 s of playback, paced to realtime: 441 pops are 10 ms of audio,
        // and each 10 ms of audio is paid with a 10 ms sleep. The device is
        // the pacemaker in production; an unpaced CPU-speed dry-run would
        // outrun the pump thread itself, starving the tee by scheduling
        // alone regardless of the fold fix.
        let mut mid = None;
        for i in 0..(44_100 * 3) {
            let _ = source.next();
            if i % 441 == 0 {
                std::thread::sleep(Duration::from_millis(10));
            }
            if i == 44_100 {
                mid = Some(bands.lock().unwrap().updated_at);
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        let mid = mid.expect("mid marker");
        let end = bands.lock().unwrap().updated_at;
        assert!(
            end > mid,
            "FFT feed died while unplayed audio was still buffered"
        );
        let peak = bands
            .lock()
            .unwrap()
            .values
            .iter()
            .copied()
            .fold(0.0f32, f32::max);
        assert!(peak > 0.0, "bands never fed at all (peak 0)");
    }

    /// Oracle #3: the FFT tee must keep feeding the band cell AFTER the
    /// prebuffer gate has opened and music is audibly playing. A freeze here
    /// (feed alive pre-start, dead once `started` flips) would reproduce the
    /// user's 2026-08-16 symptom: "visualizer moves, then stops as soon as the
    /// music starts" — with a local file there is no network to blame.
    #[test]
    #[ignore]
    fn fft_tee_keeps_feeding_once_music_is_audible() {
        let wav = std::env::temp_dir().join("tuna-tui-oracle-tone-4s.wav");
        let st = std::process::Command::new(crate::config::get().ffmpeg_path.as_str())
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=4",
                "-ac",
                "2",
                "-ar",
                "44100",
                wav.to_str().unwrap_or("/tmp/tone4s.wav"),
            ])
            .status()
            .expect("ffmpeg gen");
        assert!(st.success(), "tone generation failed");

        let (sink, player, queue_in) = open_output().expect("open device");
        let _sink = sink;
        player.play();
        // Silent by policy (headphones): volume 0 keeps the FFT tee fed
        // through the real device path without an audible test.
        player.set_volume(0.0);
        let (mut child, stdout) = spawn_ffmpeg(wav.to_str().unwrap(), 0).expect("spawn ffmpeg");
        let bands = VisBands::shared();
        let frames = Arc::new(AtomicU64::new(0));
        let cancelled = Arc::new(AtomicBool::new(false));
        let source = FfmpegSource::new(stdout, Arc::clone(&frames), Arc::clone(&bands), cancelled);
        queue_in.append_with_signal(source);
        // Prebuffer + playback start (~1.2 s is well past the 93 ms gate).
        std::thread::sleep(Duration::from_millis(1200));
        let first_update = bands.lock().unwrap().updated_at;
        let peak1 = {
            let g = bands.lock().unwrap();
            g.values.iter().copied().fold(0.0f32, f32::max)
        };
        assert!(peak1 > 0.0, "bands flat at t=1.2s (peak {peak1})");
        // Audible playback continues for another 1.5 s; the tee must keep
        // refreshing the cell (a stale `updated_at` = frozen spectrum).
        std::thread::sleep(Duration::from_millis(1500));
        let fresh = bands.lock().unwrap().updated_at > first_update;
        let peak2 = {
            let g = bands.lock().unwrap();
            g.values.iter().copied().fold(0.0f32, f32::max)
        };
        let _ = child.kill();
        let _ = child.wait();
        assert!(fresh, "bands stopped updating during audible playback");
        assert!(
            peak2 > 0.0,
            "bands fell to zero during audible playback: {peak2}"
        );
    }
}
