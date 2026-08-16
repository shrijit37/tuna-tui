//! A rodio `Source` that plays raw interleaved s16 PCM from an ffmpeg child's
//! stdout pipe.
//!
//! `ffmpeg -v error -ss <pos> -i <url> -f s16le -ac 2 -ar 44100 -` is spawned
//! by the engine; the child's stdout is handed to this source, which the audio
//! device's thread pulls through rodio's queue/player chain. Every read chunk
//! is tee'd into the shared [`Visualizer`] bands on the way past — the same
//! "tee'd sink" design the librespot backend used, minus librespot.
//!
//! # Why a pump thread
//!
//! The pipe is read by a *pump thread*, never by the audio callback. A
//! blocking `read()` in `next()` wedges cpal's callback thread the instant
//! ffmpeg's pipe is momentarily empty, and the device starts faulting
//! (`Buffer underrun/overrun` storm). The source instead drains a channel
//! without ever blocking; an empty channel (ffmpeg still starting, or jitter)
//! yields silence, so the device keeps streaming.
//!
//! A small prebuffer (≈93 ms) is filled before playback hands over the first
//! real sample, so ffmpeg's bursty start-up never reaches the listener as a
//! hole. Position authority stays the playhead: `frames` counts *stereo frames
//! actually popped* (converted from a chunk, not received) and position is
//! `start_ms + frames/44.1`.
//!
//! EOF is the pipe closing: the pump exits with `eof = true`, the source pops
//! the remaining buffer, then `Iterator::next` returns `None` and rodio's
//! queue fires the sound-done signal the engine awaits.

use std::collections::VecDeque;
use std::io::Read;
use std::process::ChildStdout;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rodio::{ChannelCount, Sample, SampleRate, Source};

use crate::audio::{VisBands, Visualizer};

/// Bytes per pump read (16 KiB = 4096 stereo frames ≈ 93 ms).
const READ_BYTES: usize = 16 * 1024;
/// Raw samples (floats) to accumulate before the first pop — the prebuffer.
/// 8 KiB of float samples = 4096 stereo frames ≈ 93 ms of latency.
const PREBUFFER_SAMPLES: usize = 8 * 1024;

/// The rodio source for one ffmpeg child. Cheap to build; the expensive pipe
/// I/O happens on the pump thread started inside [`FfmpegSource::new`].
pub(crate) struct FfmpegSource {
    /// Converted chunks from the pump, in delivery order.
    pending: VecDeque<f32>,
    /// Stereo frames delivered so far (the playhead authority).
    frames: Arc<AtomicU64>,
    /// The FFT tee writing to the renderer's shared bands.
    visualizer: Visualizer,
    /// Set when the pump saw the pipe close. After the pending pool drains
    /// past this, `next()` reports EOF rather than playing silence forever.
    eof: bool,
    /// The engine kills this source by flipping the flag ahead of the child:
    /// `next()` then ends immediately instead of draining buffered PCM at
    /// device speed — a swapped/seeked-away track must not keep sounding.
    cancelled: Arc<AtomicBool>,
    /// Prebuffer is a start-up concern only: once the first real sample has
    /// been served, later delivery gaps play silence directly instead of
    /// re-gating and stalling the queue.
    started: bool,
    /// Live chunks from the pump thread. Bounded to ~8 reads (≈0.75 s of
    /// audio): the pump blocks on a full queue — on *its own* thread, never on
    /// the audio callback — instead of letting a burst decode flood memory.
    chunks: flume::Receiver<Vec<u8>>,
}

impl FfmpegSource {
    /// Build the source over ffmpeg's stdout. `bands` is the shared cell the
    /// FFT tee writes into (the same `Arc` the renderer reads); `cancelled`
    /// is owned by the engine, which flips it to abort this source.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        stdout: ChildStdout,
        frames: Arc<AtomicU64>,
        bands: Arc<Mutex<VisBands>>,
        cancelled: Arc<AtomicBool>,
    ) -> Self {
        let (tx, chunks) = flume::bounded(8);
        // The pump: blocking `read`s live here, never on cpal's callback
        // thread. It ends when the pipe closes (natural EOF or a killed
        // child), or when the source (and so `tx`) goes away.
        std::thread::Builder::new()
            .name("ffmpeg-pump".into())
            .spawn(move || {
                let mut stdout = stdout;
                let mut buf = [0u8; READ_BYTES];
                loop {
                    match stdout.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if tx.send(buf[..n].to_vec()).is_err() {
                                break; // the source is gone
                            }
                        }
                    }
                }
                // The end-marker: read() itself never yields an empty chunk,
                // so an empty Vec is unambiguous. Without it the source would
                // play silence forever (eof never flips, the done-signal never
                // fires, the track never ends).
                let _ = tx.send(Vec::new());
            })
            .expect("spawn ffmpeg pump");
        Self {
            pending: VecDeque::with_capacity(PREBUFFER_SAMPLES),
            frames,
            visualizer: Visualizer::new(bands, 44_100.0),
            eof: false,
            cancelled,
            started: false,
            chunks,
        }
    }

    /// Fold what the pump has delivered into `pending`, teeing into the
    /// visualizer, non-blocking. The empty end-marker flips `eof` (see the
    /// pump thread).
    ///
    /// The pull is *bounded*: `pending` is kept at prebuffer depth, never
    /// grown greedily. Draining the whole channel on every call let the pump
    /// outrun playback — instant for local files, bursty for network — hit
    /// EOF, and freeze the FFT tee while `pending` still held seconds of
    /// audio (the 2026-08-16 "visualizer moves, then stops as the music
    /// starts" bug). Pushing only when the pool is low keeps delivery paced
    /// to the playhead, so the tee stays fed for the whole track, and the
    /// channel's backpressure moves the pump block off the callback thread.
    fn fold(&mut self) {
        while self.pending.len() < PREBUFFER_SAMPLES {
            match self.chunks.try_recv() {
                Ok(chunk) if !chunk.is_empty() => {
                    let ints: Vec<i16> = chunk
                        .chunks_exact(2)
                        .map(|c| i16::from_le_bytes([c[0], c[1]]))
                        .collect();
                    self.visualizer.feed_interleaved(&ints);
                    self.pending
                        .extend(ints.iter().map(|&s| s as f32 * (1.0 / 32768.0)));
                }
                Ok(_) => {
                    // The empty end-marker: pipe closed, no more audio. It is
                    // only consumed once the pool dips below the target, which
                    // the drain always does — `eof_signals` still fires when
                    // the last buffered sample is popped.
                    self.eof = true;
                    break;
                }
                Err(_) => break, // nothing delivered yet — play silence this tick
            }
        }
    }

    fn eof_signals(&self) -> bool {
        self.eof && self.chunks.is_empty()
    }
}

impl Iterator for FfmpegSource {
    type Item = Sample;

    fn next(&mut self) -> Option<Self::Item> {
        // Killed ahead of the child (track swapped/seeked away): end now.
        // Draining the buffered PCM at device speed would keep the old song
        // audible for the channel's whole backlog (~1.5 s after a busy tool).
        if self.cancelled.load(Ordering::Relaxed) {
            return None;
        }
        self.fold();
        // Prebuffer: hold back real audio until there's ~93 ms of it, so the
        // device never tears on ffmpeg's start-up. Silence here is *not*
        // counted against the playhead — and the gate is startup-only, so a
        // delivery gap later in the track plays silence directly instead of
        // re-arming the stall.
        if !self.started {
            if self.pending.len() < PREBUFFER_SAMPLES && !self.eof {
                return Some(0.0);
            }
            self.started = true;
        }
        match self.pending.pop_front() {
            Some(s) => {
                // One float = one channel sample; every second pop completes
                // a stereo frame. Counted here — delivered to the device — so
                // a fast local decode can't race the playhead ahead of what
                // has actually reached the listener.
                if self.pending.len().is_multiple_of(2) {
                    self.frames.fetch_add(1, Ordering::Relaxed);
                }
                Some(s)
            }
            None if self.eof_signals() => None,
            // Channels raced ahead of fold (awaiting the pump) — play silence
            // rather than blinking the device.
            None => Some(0.0),
        }
    }
}

impl Source for FfmpegSource {
    /// Raw PCM has no intrinsic span; `None` keeps the queue's span open until
    /// the iterator itself ends (pipe close).
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        ChannelCount::new(2).expect("stereo")
    }

    fn sample_rate(&self) -> SampleRate {
        SampleRate::new(44_100).expect("44.1 kHz")
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}
