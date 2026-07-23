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
