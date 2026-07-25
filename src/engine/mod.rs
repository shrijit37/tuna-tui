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
//! backoff the old reconnect loop used. A third thread ("tuna-meta") is the
//! single metadata worker: it drains a bounded FIFO of per-track meta jobs
//! (the worker drops the oldest job when the queue saturates, so the current
//! track's job always lands), fetches cover + theme and ships `EngineMeta` to
//! the app over a bounded channel with the same drop-oldest rule, then exits
//! when the engine goes away. The engine needs no tokio runtime.

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

/// A queued metadata job: fetch cover + theme for `uri` and ship `EngineMeta`
/// for it. `fresh` distinguishes a newly-started track (must ship) from a
/// recovery rebuild of an already-delivered one — the worker drops
/// `fresh: false` jobs without running `engine_meta`, because the app already
/// applied that track's meta on the first delivery and re-applying it would
/// re-run `record_played` (Home-count inflation).
struct MetaJob {
    uri: String,
    info: ResolvedTrack,
    fresh: bool,
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

/// A paused track's stream stash: everything needed to restart the decoder on
/// the SAME already-resolved URL (no network re-resolve) at a recorded
/// position, after `Cmd::Pause` tore the stream down. Deliberately NOT
/// [`Worker::recovery`] — `run` treats recovery as an in-flight rebuild and
/// would rebuild the stream on the next command; a stash must be invisible to
/// that logic.
struct PausedTrack {
    uri: String,
    url: String,
    duration_ms: Option<u32>,
    position_ms: u32,
}

/// The cell the watchdog polls. Only `playing` + `last_progress` matter: a
/// track that claims to play but hasn't advanced frames is a stall.
struct Health {
    playing: bool,
    last_progress: Instant,
}

impl Engine {
    /// The one `Cmd::Load` construction all play-entry points share.
    fn load(
        &self,
        tracks: Vec<String>,
        start_uri: Option<String>,
        position_ms: u32,
        shuffle: bool,
    ) -> Result<()> {
        self.send(Cmd::Load {
            tracks,
            start_uri,
            position_ms,
            shuffle,
        })
    }

    /// Start a context (playlist / album / artist / track URI). When
    /// `shuffle` is set, the whole expanded context shuffles locally.
    pub fn play_context(&self, context_uri: impl Into<String>, shuffle: bool) -> Result<()> {
        let uri = context_uri.into();
        let tracks = self.inner.expander.expand(&uri).map_err(|e| anyhow!(e))?;
        self.load(tracks, None, 0, shuffle)
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
        self.load(tracks, track_uri, position_ms, shuffle)
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
        self.load(tracks, start_uri, start_position_ms, shuffle)
    }

    /// Load a single track and start playing at `position_ms` — used to resume
    /// the last session's track when the user first hits play.
    pub fn play_track_at(&self, uri: String, position_ms: u32) -> Result<()> {
        self.load(vec![uri.clone()], Some(uri), position_ms, false)
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
    /// `cancel` is the per-request F13 flag: when the app's radio deadline has
    /// given up, the flag stops the yt-dlp chain from spawning further
    /// children instead of leaving it orphaned for ~40s.
    pub fn radio_tracks(&self, seed: &str, cancel: Arc<AtomicBool>) -> Result<Vec<String>, String> {
        self.inner.expander.radio(seed, cancel)
    }

    /// The radio station as full rows (uri + title + artist + thumbnail),
    /// seed first. The UI caches the metadata so queue rows render
    /// "Title — Artist" before each track's own resolve lands.
    pub fn radio_entries(
        &self,
        seed: &str,
        cancel: Arc<AtomicBool>,
    ) -> Result<Vec<crate::yt::YtVideo>, String> {
        self.inner.expander.radio_entries(seed, cancel)
    }
    /// Drain title/artist hints recorded during expansion/radio.
    pub fn take_meta_hints(&self) -> Vec<(String, String, String)> {
        self.inner.expander.take_meta_hints()
    }

    /// Feed YT Music search rows into the hint store.
    pub fn record_song_hints(&self, songs: &[crate::providers::contracts::Song]) {
        self.inner.expander.record_song_hints(songs);
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

    /// The length of the loaded list — the clone-free gate for the sync tick's
    /// queue refresh (`refresh_needed` in the binary): a length change is
    /// exactly when the Queue view's labels need re-formatting.
    pub fn queue_len(&self) -> usize {
        self.inner.queue.lock().map(|q| q.len()).unwrap_or_default()
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
///
/// The same bound caps `recover_into`'s rebuild loop: both are "how hard do
/// we keep retrying this track before walking away" — one number, so tuning
/// it can't leave the two paths disagreeing about when to give up.
const RECOVERY_ATTEMPTS: u32 = 8;
/// First and last wait between failed recovery attempts, so an offline spell
/// doesn't hammer the resolver every five seconds until dawn.
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(120);
/// The bounded FIFO of pending metadata jobs (F6). 16 is headroom for a
/// skip-burst at one job per track; the tuna-meta worker drains it at
/// network+decode speed, and drop-oldest keeps the current track's job from
/// ever waiting behind saturated work.
const META_JOBS_CAP: usize = 16;

/// Worker loop wakeup: drains commands, watches for EOF, emits position.
const TICK: Duration = Duration::from_millis(100);
/// How often the worker emits a [`EngineEvent::PositionCorrection`] while
/// playing, to trim the app-side extrapolated playhead.
const POSITION_EVERY: Duration = Duration::from_secs(1);

fn next_backoff(current: Duration) -> Duration {
    crate::util::backoff_step(current, RETRY_MAX)
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
/// receives the EngineEvent stream; `meta_tx`/`meta_rx` are the app's
/// in-band metadata channel (bounded, drop-oldest) — the engine keeps a
/// sending clone for its single "tuna-meta" worker and mirrors the receiver
/// so a saturated channel sheds its oldest message instead of piling up
/// multi-MB images.
pub fn run(
    events: flume::Sender<EngineEvent>,
    meta_tx: flume::Sender<EngineMeta>,
    meta_rx: flume::Receiver<EngineMeta>,
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
    // The single persistent metadata worker (F6): one "tuna-meta" thread
    // drains the bounded FIFO job queue and ships EngineMeta with
    // drop-oldest — replacing a detached thread per track start and per
    // recovery rebuild.
    let (meta_jobs_tx, meta_jobs_rx) = flume::bounded::<MetaJob>(META_JOBS_CAP);
    spawn_tuna_meta(meta_jobs_rx.clone(), meta_tx.clone(), meta_rx);
    let worker = Worker {
        sink,
        player,
        queue,
        bands: Arc::clone(&bands),
        events,
        // Keep-alive only: build_stream hands jobs to the tuna-meta worker,
        // which holds the sending clone. While this sender lives the meta
        // channel cannot disconnect, so a worker that dies mid-session
        // degrades to "no meta" — never a busy-spin on the app's select arm.
        meta_tx,
        meta_jobs: meta_jobs_tx,
        meta_jobs_rx,
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
        pending: None,
        recovery: None,
        paused: None,
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

/// Send `msg` without ever blocking; a saturated bounded channel sheds its
/// OLDEST queued message to make room (drop-oldest), then retries until the
/// send lands or there is nothing left to shed. The porch rule for the meta
/// pipeline: the current track's message must always land, older tracks'
/// messages are disposable (the app's `meta_is_current` guard makes a dropped
/// message invisible beyond that track's fallback), and a blocking send would
/// park one thread per stuck message — each holding a multi-MB image.
fn send_drop_oldest<T>(tx: &flume::Sender<T>, rx: &flume::Receiver<T>, mut msg: T) {
    loop {
        match tx.try_send(msg) {
            Ok(()) => return,
            Err(flume::TrySendError::Full(m)) => {
                msg = m;
                match rx.try_recv() {
                    // Dropped the oldest, or a receiver drained the queue
                    // concurrently — either way there is room now; retry.
                    Ok(_) | Err(flume::TryRecvError::Empty) => {}
                    // Receiver gone — nothing left to drop for.
                    Err(flume::TryRecvError::Disconnected) => return,
                }
            }
            Err(flume::TrySendError::Disconnected(_)) => return,
        }
    }
}

/// The one metadata worker (F6): drains the bounded FIFO [`MetaJob`] queue,
/// computes in-band metadata for each fresh job and ships it over the bounded
/// meta channel (drop-oldest on saturation) — one thread instead of a
/// detached thread per track start and per recovery rebuild. `fresh: false`
/// jobs (recovery re-deliveries of an already-delivered track) are skipped:
/// re-applying would re-run `record_played`, inflating Home counts. Known
/// edge (documented in the PR): a recovery that fires before the first
/// meta delivery for a track — e.g. the very first `build_stream` fails and
/// a recovery rebuild succeeds — skips meta for that track, which then falls
/// back to the bare-URI defaults until it is replayed. Exits when the engine
/// drops its job sender (the queue disconnects).
fn spawn_tuna_meta(
    jobs: flume::Receiver<MetaJob>,
    meta_tx: flume::Sender<EngineMeta>,
    meta_rx: flume::Receiver<EngineMeta>,
) {
    // Cloning the once-built blocking client (Arc-fee) — constructed by
    // `httpcache::warm_blocking_client` before the runtime started.
    let client = crate::httpcache::blocking_client().clone();
    if std::thread::Builder::new()
        .name("tuna-meta".to_string())
        .spawn(move || {
            while let Ok(job) = jobs.recv() {
                if !job.fresh {
                    // Recovery rebuild of an already-delivered track.
                    continue;
                }
                let meta = engine_meta(&job.uri, &job.info, &client);
                send_drop_oldest(&meta_tx, &meta_rx, meta);
            }
        })
        .is_err()
    {
        liblog("engine: failed to spawn tuna-meta worker");
    }
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
    /// Keep-alive for the app's in-band metadata channel — see `run`.
    /// The actual sending clone lives on the tuna-meta worker thread.
    #[allow(dead_code)] // the guard's whole job is to be held, never read
    meta_tx: flume::Sender<EngineMeta>,
    /// The bounded FIFO feeding the single tuna-meta worker (F6). Jobs are
    /// queued by `build_stream` with drop-oldest on saturation; the receiver
    /// half is how the oldest queued job is shed.
    meta_jobs: flume::Sender<MetaJob>,
    meta_jobs_rx: flume::Receiver<MetaJob>,
    cmds: flume::Receiver<Cmd>,
    expander: Arc<dyn Expander>,
    /// The public mirror of the loaded list (`Engine::queue`).
    queue_snapshot: Arc<Mutex<Vec<String>>>,
    state: PlayerState,
    current: Option<CurrentTrack>,
    health: Arc<Mutex<Health>>,
    /// Consecutive short-EOF drops on the current track (mirrors
    /// [`RECOVERY_ATTEMPTS`]); reset on a natural end or a new track.
    drop_streak: u32,
    /// The last frame count seen (stall detection needs deltas).
    last_seen_frames: u64,
    last_correction: Instant,
    /// A user command that pre-empted a recovery retry-sleep; handled before
    /// anything queued behind it.
    pending: Option<Cmd>,
    /// The track a recovery is rebuilding, while `current` is nowhere. The
    /// watchdog and a pre-empted resume use it to re-enter the rebuild loop
    /// instead of losing the track.
    recovery: Option<(String, u32)>,
    /// The paused track's stream, while `current` is nowhere: the URL stays
    /// cached so a resume can restart without re-resolving. Cleared on every
    /// transition out of the pause state (next/prev/stop/load/teardown) so it
    /// can never resurrect a stale stream on a later resume.
    paused: Option<PausedTrack>,
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
                if !self.state.playing && self.current.is_none() {
                    // Resume from the pause stash: restart the decoder on the
                    // same already-resolved URL — no network re-resolve. The
                    // stash is taken (cleared) on every outcome so a failed
                    // restart can never resurrect it.
                    let Some(p) = self.paused.take() else {
                        return;
                    };
                    let pos = truncate_seconds(p.position_ms);
                    match self.restart_stream(&p.url, &p.uri, p.duration_ms, pos, true) {
                        Ok(()) => {
                            self.state.playing = true;
                            self.set_health(true);
                            self.set_active(true);
                            // A fresh post-resume decoder starts with a clean
                            // slate, like seek_now and start_track_at give it —
                            // the pre-pause streak must not count against it.
                            self.drop_streak = 0;
                            let _ = self.events.send(EngineEvent::Playing {
                                uri: p.uri,
                                position_ms: pos,
                            });
                        }
                        Err(e) => {
                            liblog(format!("engine: resume failed: {e}"));
                            // `state.playing` is false while paused, so the
                            // rebuild stays paused; after RECOVERY_ATTEMPTS
                            // give_up_on skips the track — the audit's
                            // prescribed fallback, recover_into unchanged.
                            self.recover_into(p.uri, pos);
                        }
                    }
                } else if !self.state.playing && self.current.is_some() {
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
                    // Stash the resolved stream, then tear it down: ffmpeg and
                    // its googlevideo connection must not stay resident for the
                    // whole pause. Resume restarts from this stashed URL — no
                    // re-resolve.
                    if let Some(cur) = &self.current {
                        self.paused = Some(PausedTrack {
                            uri: cur.uri.clone(),
                            url: cur.url.clone(),
                            duration_ms: cur.duration_ms,
                            position_ms: pos,
                        });
                    }
                    self.shutdown_current();
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
                // The stash counts as the current track: the >5s restart rule
                // and the no-history restart-at-0 both apply to a paused
                // track too (seek_now updates the stash, staying paused).
                let pos = self.current_pos();
                if (self.current.is_some() || self.paused.is_some()) && pos > 5_000 {
                    self.seek_now(0);
                } else if let Some(prev) = self.state.history.pop() {
                    self.start_track_at(prev, 0);
                } else if self.current.is_some() || self.paused.is_some() {
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
            Cmd::Stop => self.stop_playback(),
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
            // While paused the stash owns the playhead: `prev` consults it so
            // the pre-teardown contract (restart the current track at 0 when
            // past 5s, stay paused) survives the teardown.
            None => self.paused.as_ref().map_or(0, |p| p.position_ms),
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
            let pick = shuffle_pick(self.state.cursor, n, &mut rand::rng());
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
                // Queue over, repeat off: stop cleanly, like the old endpoint. The loaded
                // queue is kept — prev/next still navigate the list after a stop.
                self.shutdown_current();
                self.stop_tail();
            }
        }
    }
    /// The natural end of the current track (EOF): advance the queue; if the
    /// process died instead of ending, rebuild the stream.
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
    /// Rebuilds are bounded by [`RECOVERY_ATTEMPTS`]: each drop on the same track
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
        let dropped = is_stream_dropped(pos, cur.duration_ms, failed);
        if dropped {
            let _ = cur.child.kill();
            let _ = cur.child.wait();
            self.drop_streak += 1;
            if self.drop_streak >= RECOVERY_ATTEMPTS {
                liblog(format!(
                    "engine: giving up on {uri} after {RECOVERY_ATTEMPTS} consecutive failed EOFs"
                ));
                self.give_up_on(uri);
                return;
            }
            if failed {
                liblog(format!("engine: decoder died for {uri}; rebuilding stream"));
            } else {
                liblog(format!(
                    "engine: stream dropped for {uri} at {pos}ms (expected {:?}ms); rebuilding",
                    cur.duration_ms
                ));
            }
            self.recover_into(uri, pos);
            return;
        }
        self.drop_streak = 0;
        drop(cur);
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
            "engine: giving up on {uri} after {RECOVERY_ATTEMPTS} consecutive failed EOFs"
        ));
        if self.state.tracks.is_empty() {
            self.stop_tail();
        } else if dead < self.state.tracks.len() {
            self.start_track_at(dead, 0);
        } else if self.state.repeat {
            self.start_track_at(0, 0);
        } else {
            self.stop_tail();
        }
    }

    /// A watchdog stall or a failing decoder: re-resolve and restart from the
    /// current position, with the old 5–120 s backoff. Gives up (skips the
    /// track) after [`RECOVERY_ATTEMPTS`].
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
        for attempt in 0..RECOVERY_ATTEMPTS {
            if attempt > 0 {
                let _ = self.events.send(EngineEvent::Reconnecting);
            }
            match self.build_stream(&uri, pos, play, false) {
                Ok(()) => {
                    if attempt > 0 {
                        let _ = self.events.send(EngineEvent::Reconnected);
                    }
                    self.recovery = None;
                    return;
                }
                Err(e) => {
                    liblog(format!("engine: recover {uri} attempt {attempt}: {e}"));
                    if attempt + 1 >= RECOVERY_ATTEMPTS {
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

    /// Stop playback and reset every track-related state: cancel the current
    /// child, clear the queue + mirror + history, and announce `Stopped`.
    fn stop_playback(&mut self) {
        self.shutdown_current();
        self.recovery = None;
        self.paused = None; // a stop during pause must not resurrect the stash
        if let Ok(mut q) = self.queue_snapshot.lock() {
            q.clear();
        }
        self.state.tracks.clear();
        self.state.history.clear();
        self.state.cursor = 0;
        self.stop_tail();
    }

    /// The shared teardown tail of every stop path: mark the player stopped
    /// (watchdog off, bands zeroed) and announce it. Callers run their own
    /// prelude — shut down the current child, and (only [`stop_playback`])
    /// clear the loaded queue.
    fn stop_tail(&mut self) {
        self.state.playing = false;
        self.set_health(false);
        self.set_active(false);
        self.reset_bands();
        self.paused = None; // every stop tail is a transition out of pause
        let _ = self.events.send(EngineEvent::Stopped);
    }

    fn interruptible_sleep(&self, dur: Duration) -> Option<Cmd> {
        let deadline = Instant::now() + dur;
        while Instant::now() < deadline {
