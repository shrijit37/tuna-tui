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
//! A prebuffer (`config.buffer_duration_secs`, default 2 s — the legacy
//! constant was ≈93 ms) is filled before playback hands over the first real
//! sample, so ffmpeg's bursty start-up — or a high-latency link — never
//! reaches the listener as a hole. Position authority stays the playhead:
//! `frames` counts *stereo frames actually popped* (converted from a chunk,
//! not received) and position is `start_ms + frames/44.1`.
//!
//! EOF is the pipe closing: the pump exits with `eof = true`, the source pops
//! the remaining buffer, then `Iterator::next` returns `None` and rodio's
//! queue fires the sound-done signal the engine awaits.

use std::collections::VecDeque;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rodio::{ChannelCount, Sample, SampleRate, Source};

use crate::audio::{VisBands, Visualizer};

/// Bytes per pump read (16 KiB = 4096 stereo frames ≈ 93 ms).
const READ_BYTES: usize = 16 * 1024;
/// Float samples per pump chunk (one s16 read = 8192 floats). `fold` pulls
/// whole chunks, so the pump channel must hold *at least* the whole pre-roll
/// in chunks — rounding down leaves the gate a chunk short of its threshold
/// and the source playing silence one pop past the moment it could start.
fn prebuffer_capacity(prebuffer_samples: usize) -> usize {
    // Round UP: any pre-roll that isn't an exact chunk multiple needs the
    // partial chunk's worth of depth too. The 8-chunk floor keeps the legacy
    // depth for tiny buffers.
    prebuffer_samples.div_ceil(READ_BYTES / 2).max(8)
}

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
    /// Live chunks from the pump thread. Bounded to hold the full pre-roll
    /// (≈0.75 s at the legacy 93 ms depth; `prebuffer_samples` bytes at
    /// configurable depths): the pump blocks on a full queue — on *its own*
    /// thread, never on the audio callback — instead of letting a burst
    /// decode flood memory.
    chunks: flume::Receiver<Vec<u8>>,
    /// Reused s16 decode buffer, one pump chunk in size (4096 i16 = READ_BYTES).
    /// `fold` decodes into it instead of allocating a Vec per chunk.
    scratch: Vec<i16>,
    /// The pre-roll depth in float samples: the gate threshold in `next()`,
    /// the pull bound in `fold()`, and the `pending` capacity. Threaded in
    /// from `config.buffer_duration_secs` by the engine.
    prebuffer_samples: usize,
    /// Stereo frames DECODED from the pump (fold), as opposed to delivered
    /// (`frames`). While the gate holds pops, only this counter moves — the
    /// engine's watchdog treats "either counter advancing" as liveness, so a
    /// slow-but-working link filling a deep pre-roll is never mistaken for a
    /// stall and torn down.
    sourced: Arc<AtomicU64>,
}

impl FfmpegSource {
    /// Build the source over ffmpeg's stdout. `bands` is the shared cell the
    /// FFT tee writes into (the same `Arc` the renderer reads); `cancelled`
    /// is owned by the engine, which flips it to abort this source. Generic
    /// over the reader so the gate tests can drive it with a fake PCM pipe.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new<R: Read + Send + 'static>(
        stdout: R,
        frames: Arc<AtomicU64>,
        sourced: Arc<AtomicU64>,
        bands: Arc<Mutex<VisBands>>,
        cancelled: Arc<AtomicBool>,
        prebuffer_samples: usize,
    ) -> Self {
        // The channel must hold at least the whole pre-roll in chunks —
        // otherwise `fold` drains it once, stays below the gate, and the
        // source plays silence until EOF: a startup deadlock.
        let (tx, chunks) = flume::bounded(prebuffer_capacity(prebuffer_samples));
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
            pending: VecDeque::with_capacity(prebuffer_samples),
            frames,
            visualizer: Visualizer::new(bands, 44_100.0),
            eof: false,
            cancelled,
            started: false,
            chunks,
            scratch: vec![0i16; READ_BYTES / 2],
            prebuffer_samples,
            sourced,
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
        while self.pending.len() < self.prebuffer_samples {
            match self.chunks.try_recv() {
                Ok(chunk) if !chunk.is_empty() => {
                    // Decode into the reused scratch (chunks_exact drops a
                    // trailing odd byte the same way the old collect did).
                    let n = chunk.len() / 2;
                    for (i, pair) in chunk.chunks_exact(2).enumerate() {
                        self.scratch[i] = i16::from_le_bytes([pair[0], pair[1]]);
                    }
                    self.visualizer.feed_interleaved(&self.scratch[..n]);
                    // Decode-side progress: the playhead authority is pops,
                    // but the watchdog's liveness signal is "is the decoder
                    // producing" — count what the pump delivered, so a deep
                    // pre-roll filling slowly is alive, not stalled.
                    self.sourced.fetch_add((n / 2) as u64, Ordering::Relaxed);
                    self.pending.extend(
                        self.scratch[..n]
                            .iter()
                            .map(|&s| s as f32 * (1.0 / 32768.0)),
                    );
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
        // Prebuffer: hold back real audio until the configured depth is
        // filled, so the device never tears on ffmpeg's start-up or a
        // high-latency link. Silence here is *not* counted against the
        // playhead — and the gate is startup-only, so a delivery gap later
        // in the track plays silence directly instead of re-arming the stall.
        if !self.started {
            if self.pending.len() < self.prebuffer_samples && !self.eof {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Interleaved s16 stereo PCM: `frames` stereo frames of nonzero samples
    /// (`i + 1` skips zero so no real pop is ever classified as silence, and
    /// wraps only past 32_768, far above any test's frame count).
    fn pcm(frames: usize) -> Vec<u8> {
        (0..frames * 2)
            .flat_map(|i| ((i as i16) + 1).to_le_bytes())
            .collect()
    }

    /// A reader that parks until released, then serves `data`, then either
    /// reports EOF (like a pipe closing) or parks forever (a slow pipe that
    /// never closes). The pump thread blocks in `read()` until `release`
    /// flips — which gives the tests a deterministic "before any audio"
    /// phase to prove the gate's silence is uncounted.
    struct GatedReader {
        data: Vec<u8>,
        pos: usize,
        release: Arc<AtomicBool>,
        eof: bool,
    }

    impl Read for GatedReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            // Hold the pump until the test lets the audio through.
            while !self.release.load(Ordering::SeqCst) {
                std::thread::yield_now();
            }
            if self.pos < self.data.len() {
                let n = (self.data.len() - self.pos).min(buf.len());
                buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
                self.pos += n;
                Ok(n)
            } else if self.eof {
                Ok(0) // natural EOF: the pump breaks and sends the end-marker
            } else {
                std::thread::park(); // the pump waits here; the test is done
                Ok(0) // spurious unpark only — treated as EOF, harmless
            }
        }
    }

    /// A source over a gated pipe with the given pre-roll depth. The release
    /// flag lets the test hold the pipe shut to observe the silence phase;
    /// `eof` says whether the pipe closes after its data (test 1) or stays
    /// open for a delivery gap (tests 2/3).
    fn gated_source(
        prebuffer_samples: usize,
        data: Vec<u8>,
        eof: bool,
    ) -> (
        FfmpegSource,
        Arc<AtomicU64>,
        Arc<AtomicU64>,
        Arc<AtomicBool>,
    ) {
        let frames = Arc::new(AtomicU64::new(0));
        let sourced = Arc::new(AtomicU64::new(0));
        let release = Arc::new(AtomicBool::new(false));
        let src = FfmpegSource::new(
            GatedReader {
                data,
                pos: 0,
                release: Arc::clone(&release),
                eof,
            },
            Arc::clone(&frames),
            Arc::clone(&sourced),
            VisBands::shared(),
            Arc::new(AtomicBool::new(false)),
            prebuffer_samples,
        );
        (src, frames, sourced, release)
    }

    /// Take up to `max` pops. Returns (zeros, real pops, frames at the end,
    /// ended). Use only where the pump's delivery state is already settled
    /// (pre-release silence, or a channel drained after the supplied data
    /// was consumed): a fixed pop budget races the pump thread otherwise —
    /// on a loaded box the drive can finish before the pump's first chunk or
    /// its end-marker lands, misreading a good source as "zero audio".
    fn drive(src: &mut FfmpegSource, frames: &AtomicU64, max: usize) -> (usize, usize, u64, bool) {
        let mut zeros = 0;
        let mut real = 0;
        let mut ended = false;
        for _ in 0..max {
            match src.next() {
                Some(s) => {
                    if s == 0.0 {
                        zeros += 1;
                    } else {
                        real += 1;
                    }
                }
                None => {
                    ended = true;
                    break;
                }
            }
        }
        (zeros, real, frames.load(Ordering::Relaxed), ended)
    }

    /// Drive pops until the source ends or `timeout` elapses — the pump
    /// thread delivers chunks asynchronously, so the caller must WAIT for
    /// the end-marker, not race it with a pop budget. Returns (zeros, real
    /// pops, frames at the end, ended). The timeout only bounds a genuinely
    /// broken source; a healthy pump lands its marker in milliseconds.
    fn drive_until_ended(
        src: &mut FfmpegSource,
        frames: &AtomicU64,
        timeout: Duration,
    ) -> (usize, usize, u64, bool) {
        let deadline = Instant::now() + timeout;
        let (mut zeros, mut real) = (0, 0);
        loop {
            match src.next() {
                Some(0.0) => zeros += 1,
                Some(_) => real += 1,
                None => return (zeros, real, frames.load(Ordering::Relaxed), true),
            }
            if Instant::now() >= deadline {
                return (zeros, real, frames.load(Ordering::Relaxed), false);
            }
        }
    }

    /// Drive pops until `expected` real samples were popped, the source
    /// ended, or `timeout` elapsed. Returns (real pops, frames, ended) —
    /// used where the gate's POOL opening is the invariant and no EOF ever
    /// comes (the pipe stays open for the gap phase, so "ended" stays
    /// false by design).
    fn drive_until_real(
        src: &mut FfmpegSource,
        frames: &AtomicU64,
        expected_real: usize,
        timeout: Duration,
    ) -> (usize, u64, bool) {
        let deadline = Instant::now() + timeout;
        let mut real = 0;
        loop {
            match src.next() {
                Some(0.0) => {}
                Some(_) => real += 1,
                None => return (real, frames.load(Ordering::Relaxed), true),
            }
            if real >= expected_real {
                return (real, frames.load(Ordering::Relaxed), false);
            }
            if Instant::now() >= deadline {
                return (real, frames.load(Ordering::Relaxed), false);
            }
        }
    }

    /// The gate holds back real audio until the pending pool reaches the
    /// threshold — and when the pipe can never fill it, EOF opens the gate
    /// early so a short stream completes instead of hanging in silence.
    #[test]
    fn gate_holds_below_threshold_and_eof_opens_it() {
        // 2048 stereo frames = 4096 float samples against a 8192-sample gate:
        // the pool can never fill it — only the end-marker can open the gate.
        let (mut src, frames, sourced, release) = gated_source(8 * 1024, pcm(2048), true);

        // With the pipe shut, pops are silence — and uncounted.
        let (z0, r0, f0, e0) = drive(&mut src, &frames, 1000);
        assert_eq!(r0, 0, "no real audio before the pipe opens");
        assert_eq!(f0, 0, "silence must not advance the playhead");
        assert!(!e0, "no EOF while the pipe is open");
        assert!(z0 > 0, "the gate should have played some silence");

        // Release: the pipe delivers + closes. EOF opens the gate below the
        // threshold; every sample is popped, then the source ends. The
        // end-marker lands on the pump thread's schedule — wait for it
        // (bounded), never race it with a pop budget.
        release.store(true, Ordering::SeqCst);
        let (_, real, f, ended) = drive_until_ended(&mut src, &frames, Duration::from_secs(5));
        assert_eq!(real, 4096, "all delivered audio is played");
        assert_eq!(f, 2048, "only real pops count as stereo frames");
        assert!(ended, "the short stream must end after EOF");
        assert_eq!(src.next(), None, "the iterator stays ended");
        assert_eq!(
            sourced.load(Ordering::Relaxed),
            2048,
            "decode progress counts every delivered frame"
        );
    }

    /// Once the pool reaches the threshold the gate opens on its own — no EOF
    /// needed — and a later delivery gap plays (uncounted) silence instead of
    /// re-gating or ending.
    #[test]
    fn gate_opens_on_filled_pool_then_gap_plays_silence() {
        // One full chunk (8192 float samples) exactly fills an 8192-sample
        // gate; the pipe then never closes.
        let (mut src, frames, _sourced, release) = gated_source(8 * 1024, pcm(4096), false);

        release.store(true, Ordering::SeqCst);
        let (real, f, ended) = drive_until_real(&mut src, &frames, 8192, Duration::from_secs(5));
        assert_eq!(real, 8192, "gate opens on the filled pool, no EOF needed");
        assert_eq!(f, 4096, "4096 stereo frames popped");
        assert!(!ended, "a gap is silence, not EOF");

        // The channel is drained and the pipe never closes: mid-track silence.
        let (zeros, real2, f2, ended2) = drive(&mut src, &frames, 2000);
        assert_eq!(real2, 0, "no more real audio");
        assert!(zeros > 0, "gap plays silence");
        assert_eq!(f2, 4096, "gap silence is not counted");
        assert!(!ended2, "gap silence is not EOF");
    }

    /// Decode-side progress (the watchdog's liveness signal) advances while
    /// the gate still holds pops: one chunk decoded into a two-chunk gate
    /// moves `sourced` without a single delivered frame — the exact "slow
    /// link filling the pre-roll" state the watchdog must not call a stall.
    #[test]
    fn decode_progress_is_counted_while_the_gate_holds_pops() {
        // One chunk (8192 floats) against a two-chunk gate: queued audio
        // with a closed gate.
        let (mut src, frames, sourced, release) = gated_source(16 * 1024, pcm(4096), false);
        release.store(true, Ordering::SeqCst);
        // Pop until the pump's chunk is folded in (deterministic: the pump
        // thread is not scheduled on demand), counting how many pops it took.
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut real = 0usize;
        while Instant::now() < deadline {
            match src.next() {
                Some(0.0) => {} // gated silence
                Some(_) => real += 1,
                None => break,
            }
            if sourced.load(Ordering::Relaxed) >= 4096 {
                break;
            }
        }
        assert_eq!(real, 0, "the gate still holds pops");
        assert_eq!(
            sourced.load(Ordering::Relaxed),
            4096,
            "decoded frames are counted while pops are gated"
        );
        assert_eq!(
            frames.load(Ordering::Relaxed),
            0,
            "no delivered frames while the gate holds"
        );
    }

    /// The pump channel must hold the whole pre-roll in chunks: a capacity
    /// truncated below the pre-roll would leave the gate a chunk short of its
    /// threshold at every supported depth (the 1s end rounds 10.76 chunks
    /// down to 10 and loses the 11th — the gate then opens one pop late).
    #[test]
    fn channel_capacity_holds_the_full_pre_roll() {
        for (secs, depth, chunks) in [
            (1u8, 88_200usize, 11usize),
            (2, 176_400, 22),
            (5, 441_000, 54),
            (30, 2_646_000, 323),
        ] {
            let cap = prebuffer_capacity(depth);
            assert_eq!(cap, chunks, "{secs}s pre-roll needs {chunks} chunks");
            // The invariant the capacity exists for: one full channel can
            // satisfy the gate without waiting on a refill.
            assert!(
                cap * (READ_BYTES / 2) >= depth,
                "{secs}s: capacity {cap} chunks < pre-roll"
            );
        }
        // Tiny depths keep the legacy floor.
        assert_eq!(prebuffer_capacity(4096), 8);
    }

    /// The threshold is a parameter, not a constant: a bigger buffer delivers
    /// a longer gate.
    #[test]
    fn threshold_is_parameterized() {
        // Two chunks (16384 float samples) against a 16384-sample gate.
        let (mut src, frames, _sourced, release) = gated_source(16 * 1024, pcm(8192), false);

        release.store(true, Ordering::SeqCst);
        let (real, f, _) = drive_until_real(&mut src, &frames, 16_384, Duration::from_secs(5));
        assert_eq!(real, 16_384, "the full 16k-sample pool is popped");
        assert_eq!(f, 8192, "8192 stereo frames");
    }
}
