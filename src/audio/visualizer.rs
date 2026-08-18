//! Real-time FFT frequency-band visualizer.
//!
//! Vendored and adapted from aome510/spotify-player (`ui/streaming.rs`, MIT,
//! © 2021 Thang Pham), then decoupled from librespot (phase 2 of the YouTube
//! port): the feed is plain interleaved s16 PCM — whatever the decoder emits —
//! rather than librespot's `AudioPacket`. The math is unchanged; the hot path
//! is allocation-free and the UI reads the bands via `try_lock`, so the audio
//! thread never stalls waiting on a render.
//!
//! The engine's `FfmpegSource` owns one [`Visualizer`] and feeds it a copy of
//! every read chunk; `is_active` is driven by the engine's playback state.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rustfft::{num_complex::Complex, FftPlanner};

const FFT_SIZE: usize = 1024;
/// New samples consumed per FFT frame (overlap = FFT_SIZE - HOP_SIZE).
const HOP_SIZE: usize = 128;
pub const NUM_BANDS: usize = 128;

/// Per-frame decay for individual bands — snappy but not jittery.
const DECAY_FACTOR: f32 = 0.985;
/// Slower decay for the normalization envelope so quiet passages read quiet.
const DECAY_FACTOR_PEAK: f32 = 0.9985;

/// Shared frequency-band state written by the visualizer, read by the renderer.
pub struct VisBands {
    pub values: [f32; NUM_BANDS],
    pub updated_at: Instant,
    pub peak_envelope: f32,
    pub is_active: bool,
    /// Render-gated: the UI sets this from the NowPlaying view expression
    /// every tick; `feed_interleaved` early-returns while it is false (perf
    /// audit F7). Defaults to true — the tee is never off unless the view
    /// that consumes it is explicitly not on screen.
    pub enabled: bool,
}

impl VisBands {
    pub fn new() -> Self {
        Self {
            values: [0.0; NUM_BANDS],
            updated_at: Instant::now(),
            peak_envelope: 1e-6,
            is_active: false,
            enabled: true,
        }
    }

    pub fn shared() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self::new()))
    }
}

impl Default for VisBands {
    fn default() -> Self {
        Self::new()
    }
}

/// The tee'd FFT engine: forwards PCM to the shared bands while the caller
/// forwards the same PCM to the audio device. Compute stays off the renderer;
/// the renderer reads the bands via `try_lock`.
pub struct Visualizer {
    sample_buf: VecDeque<f32>,
    bands: Arc<Mutex<VisBands>>,
    fft: Arc<dyn rustfft::Fft<f32>>,
    hann_window: Vec<f32>,
    fft_buf: Vec<Complex<f32>>,
    magnitudes: Vec<f32>,
    sample_rate: f32,
    band_ranges: Vec<(usize, usize)>,
    new_bands: [f32; NUM_BANDS],
    smooth_scratch: [f32; NUM_BANDS],
}

impl Visualizer {
    pub fn new(bands: Arc<Mutex<VisBands>>, sample_rate: f32) -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let hann_window: Vec<f32> = (0..FFT_SIZE)
            .map(|i| {
                0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos())
            })
            .collect();
        let band_ranges = precompute_band_ranges(FFT_SIZE / 2, NUM_BANDS);
        Self {
            sample_buf: VecDeque::with_capacity(FFT_SIZE * 2),
            bands,
            fft,
            hann_window,
            fft_buf: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            magnitudes: vec![0.0; FFT_SIZE / 2],
            sample_rate,
            band_ranges,
            new_bands: [0.0; NUM_BANDS],
            smooth_scratch: [0.0; NUM_BANDS],
        }
    }

    /// The shared bands this visualizer writes (what the renderer reads).
    pub fn bands(&self) -> &Arc<Mutex<VisBands>> {
        &self.bands
    }

    #[cfg(test)]
    fn sample_buf_len(&self) -> usize {
        self.sample_buf.len()
    }

    /// Feed one chunk of interleaved stereo s16 PCM (whatever the decoder
    /// produced); the FFT bands update in place.
    pub fn feed_interleaved(&mut self, samples: &[i16]) {
        // Render-gated tee (perf audit F7): while no view consumes the bands
        // the whole pipeline is skipped BEFORE the extend — a gate after it
        // would grow sample_buf unboundedly and burst-lag re-enable.
        // updated_at is deliberately untouched while disabled: stale means
        // decay≈0, so the first frame after re-enable replaces the stale
        // peaks immediately (the Myx-a4.14 frozen-spectrum class must not
        // come back).
        // try_lock on the audio path (perf audit F7): a UI thread holding
        // the bands lock must never block the pump — a missed gate read just
        // means the next chunk re-checks.
        if !self.bands.try_lock().map(|g| g.enabled).unwrap_or(true) {
            return;
        }
        // Interleaved stereo -> mono.
        self.sample_buf.extend(samples.chunks(2).map(|c| {
            if c.len() == 2 {
                (c[0] as f32 + c[1] as f32) * 0.5
            } else {
                c[0] as f32
            }
        }));

        while self.sample_buf.len() >= FFT_SIZE {
            {
                let (front, back) = self.sample_buf.as_slices();
                if front.len() >= FFT_SIZE {
                    for (dst, (&s, &w)) in self
                        .fft_buf
                        .iter_mut()
                        .zip(front.iter().zip(self.hann_window.iter()))
                    {
                        *dst = Complex::new(s * w, 0.0);
                    }
                } else {
                    let split = front.len();
                    for (dst, (&s, &w)) in self.fft_buf[..split]
                        .iter_mut()
                        .zip(front.iter().zip(self.hann_window[..split].iter()))
                    {
                        *dst = Complex::new(s * w, 0.0);
                    }
                    let remaining = FFT_SIZE - split;
                    for (dst, (&s, &w)) in self.fft_buf[split..].iter_mut().zip(
                        back[..remaining]
                            .iter()
                            .zip(self.hann_window[split..].iter()),
                    ) {
                        *dst = Complex::new(s * w, 0.0);
                    }
                }
            }

            self.fft.process(&mut self.fft_buf);

            for (mag, c) in self.magnitudes.iter_mut().zip(self.fft_buf.iter()) {
                *mag = c.norm();
            }

            fill_log_bands(&self.magnitudes, &self.band_ranges, &mut self.new_bands);
            smooth_bands(&mut self.new_bands, &mut self.smooth_scratch);

            // try_lock: a dropped update is fine — the next hop re-fills the
            // bands, and the pump thread never waits on the UI.
            if let Ok(mut g) = self.bands.try_lock() {
                let elapsed_hops =
                    g.updated_at.elapsed().as_secs_f32() * self.sample_rate / HOP_SIZE as f32;
                let decay = DECAY_FACTOR.powf(elapsed_hops);
                let peak_decay = DECAY_FACTOR_PEAK.powf(elapsed_hops);
                let frame_peak = self.new_bands.iter().copied().fold(0.0_f32, f32::max);
                for (stored, fresh) in g.values.iter_mut().zip(self.new_bands.iter()) {
                    *stored = (*stored * decay).max(*fresh);
                }
                g.peak_envelope = (g.peak_envelope * peak_decay).max(frame_peak);
                g.updated_at = Instant::now();
            }

            self.sample_buf.drain(..HOP_SIZE);
        }
    }
}

fn precompute_band_ranges(num_bins: usize, num_bands: usize) -> Vec<(usize, usize)> {
    let log_min = 1.0_f64;
    let log_max = num_bins as f64;
    let mut used_up_to: usize = 1;
    let mut ranges = Vec::with_capacity(num_bands);
    for band in 0..num_bands {
        if used_up_to >= num_bins {
            ranges.push((num_bins - 1, num_bins));
            continue;
        }
        let t_start = band as f64 / num_bands as f64;
        let t_end = (band + 1) as f64 / num_bands as f64;
        let natural_start = (log_min * (log_max / log_min).powf(t_start)) as usize;
        let natural_end = (log_min * (log_max / log_min).powf(t_end)) as usize;
        let start = natural_start.max(used_up_to).min(num_bins - 1);
        let end = natural_end.max(start + 1).min(num_bins);
        used_up_to = end;
        ranges.push((start, end));
    }
    ranges
}

fn fill_log_bands(magnitudes: &[f32], band_ranges: &[(usize, usize)], out: &mut [f32]) {
    for (band_val, &(start, end)) in out.iter_mut().zip(band_ranges.iter()) {
        let len = (end - start) as f32;
        let sum_sq: f32 = magnitudes[start..end].iter().map(|&v| v * v).sum();
        *band_val = (sum_sq / len).sqrt();
    }
}

fn smooth_bands(bands: &mut [f32], scratch: &mut [f32]) {
    let n = bands.len();
    if n < 3 {
        return;
    }
    scratch[..n].copy_from_slice(&bands[..n]);
    for i in 0..n {
        let prev = scratch[if i > 0 { i - 1 } else { 0 }];
        let next = scratch[if i + 1 < n { i + 1 } else { n - 1 }];
        bands[i] = prev * 0.25 + scratch[i] * 0.5 + next * 0.25;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_defaults_to_true() {
        // Default-true pins the oracle contract: feeding must work out of
        // the box (the `fft_tee_keeps_feeding_*` family, silence_stays_quiet,
        // a_loud_tone_moves_the_low_bands — all assume the tee is on).
        let bands = VisBands::shared();
        assert!(bands.lock().unwrap().enabled);
    }

    #[test]
    fn disabled_feed_does_not_accumulate() {
        let bands = VisBands::shared();
        bands.lock().unwrap().enabled = false;
        let mut v = Visualizer::new(Arc::clone(&bands), 44_100.0);
        let residue = v.sample_buf_len();
        let before_updated = bands.lock().unwrap().updated_at;
        // A loud tone must not even enter the buffer while the tee is off.
        let tone = loud_tone();
        for chunk in tone.chunks(4096) {
            v.feed_interleaved(chunk);
        }
        {
            let g = bands.lock().unwrap();
            assert_eq!(v.sample_buf_len(), residue, "disabled feed grew the buffer");
            assert!(
                g.values.iter().all(|x| *x == 0.0),
                "disabled feed moved the bands"
            );
            assert_eq!(
                g.updated_at, before_updated,
                "disabled feed touched updated_at"
            );
        } // guard dropped: the re-lock below must not self-deadlock
          // Re-enable: the same signal must energize the bands.
        bands.lock().unwrap().enabled = true;
        for chunk in tone.chunks(4096) {
            v.feed_interleaved(chunk);
        }
        let g = bands.lock().unwrap();
        let peak = g.values.iter().copied().fold(0.0f32, f32::max);
        assert!(
            peak > 1_000.0,
            "re-enabled feed should energize the bands, got {peak}"
        );
    }

    /// 220 Hz square-ish tone at full scale, interleaved stereo (L == R) —
    /// the same signal `a_loud_tone_moves_the_low_bands` uses.
    fn loud_tone() -> Vec<i16> {
        let frame = 44_100 / 220;
        let mut tone = Vec::with_capacity(44_100 * 2);
        for i in 0..44_100 {
            let s = if (i / frame) % 2 == 0 { 32000 } else { -32000 };
            tone.push(s);
            tone.push(s);
        }
        tone
    }

    #[test]
    fn silence_stays_quiet() {
        let bands = VisBands::shared();
        let mut v = Visualizer::new(Arc::clone(&bands), 44_100.0);
        // 0.5s of digital silence must not move any band off zero.
        let silence = vec![0i16; 44_100 / 2 * 2];
        for chunk in silence.chunks(4096) {
            v.feed_interleaved(chunk);
        }
        let g = bands.lock().unwrap();
        assert!(g.values.iter().all(|x| *x == 0.0), "silence yields bands");
        assert!(!g.is_active);
    }

    #[test]
    fn a_loud_tone_moves_the_low_bands() {
        let bands = VisBands::shared();
        let mut v = Visualizer::new(Arc::clone(&bands), 44_100.0);
        // 220 Hz square-ish tone at full scale.
        let tone = loud_tone();
        for chunk in tone.chunks(4096) {
            v.feed_interleaved(chunk);
        }
        let g = bands.lock().unwrap();
        let peak = g.values.iter().copied().fold(0.0f32, f32::max);
        assert!(peak > 1_000.0, "tone should energize the bands, got {peak}");
    }
}
