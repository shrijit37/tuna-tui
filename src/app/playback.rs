//! The playhead: what's playing, its metadata, and the coalesced scrub state.

use crate::*;

pub(crate) struct NowPlaying {
    pub(crate) uri: String,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: String,
    pub(crate) duration_ms: u32,
    pub(crate) position_ms: u32,
    pub(crate) position_at: Instant,
    pub(crate) is_playing: bool,
    pub(crate) cover: Option<Cover>,
}

pub(crate) struct TrackMeta {
    pub(crate) uri: String,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: String,
    pub(crate) duration_ms: u32,
    pub(crate) image: TrackImage,
    pub(crate) theme: Option<Theme>,
}

pub(crate) struct TrackImage {
    pub(crate) url: Option<String>,
    pub(crate) image: Option<image::DynamicImage>,
}

/// What kind of thing is currently playing — persisted so we can resume the real
/// context (and its live queue) on reboot, not just a bare track.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) enum PlaySource {
    #[default]
    None,
    Context(String), // playlist / album / artist URI
    Radio(String),   // seed track URI
    Liked,
}

/// The playhead: what's playing plus the coalesced Shift+arrow scrub state.
///
/// These live together because every scrub method touches both — `seek_step`
/// reads `now.duration_ms` and writes the `seek_*` fields, and
/// `set_local_position` gates a write to `now` on `seek_target`.
pub(crate) struct PlaybackState {
    pub(crate) now: Option<NowPlaying>,
    /// When the playhead last really moved (or a track (re)started), for
    /// stall detection — see [`stall_status`].
    pub(crate) last_advance: Option<Instant>,
    // Shift+arrow scrubbing, coalesced (see `seek_step`).
    pub(crate) seek_target: Option<u32>,
    pub(crate) seek_last_step: Instant,
    pub(crate) seek_last_input: Instant,
}

/// How far one Shift+arrow press moves the playhead.
pub(crate) const SEEK_STEP_MS: i64 = 5_000;
/// Fastest a held Shift+arrow may step. macOS repeats keys ~30×/s; unthrottled,
/// a one-second hold would throw the playhead 2½ minutes down the track.
pub(crate) const SEEK_REPEAT: Duration = Duration::from_millis(200);
/// Quiet time after the last press before the scrub reaches the engine.
pub(crate) const SEEK_SETTLE: Duration = Duration::from_millis(250);
/// How long "playing" may go without any position advance before the UI
/// reports a stall. A stalled stream URL keeps the playhead frozen while the
/// engine's recovery loop quietly re-resolves — the honest status line is the
/// only sign of life the user gets (verified 2026-08-16).
pub(crate) const STALL_GRACE: Duration = Duration::from_secs(4);
/// The status line shown during a stall; cleared on the next real advance.
pub(crate) const STALL_STATUS: &str = "stalled — retrying…";

/// Has playback claimed to run without delivering any audio for longer than
/// `grace`? `None` (no advance yet) is never a stall: a fresh track is
/// allowed to buffer silently.
pub(crate) fn stall_status(
    is_playing: bool,
    last_advance: Option<Instant>,
    now: Instant,
    grace: Duration,
) -> bool {
    is_playing && last_advance.is_some_and(|t| now.duration_since(t) > grace)
}

pub(crate) fn should_apply_engine_position(from_engine: bool, seek_target: Option<u32>) -> bool {
    !(from_engine && seek_target.is_some())
}

pub(crate) fn scrub_target(from_ms: u32, duration_ms: u32, delta_ms: i64) -> u32 {
    (from_ms as i64 + delta_ms).clamp(0, duration_ms as i64) as u32
}

impl PlaybackState {
    pub(crate) fn position_ms(&self) -> u32 {
        match &self.now {
            Some(n) if n.is_playing => {
                (n.position_ms + n.position_at.elapsed().as_millis() as u32).min(n.duration_ms)
            }
            Some(n) => n.position_ms.min(n.duration_ms),
            None => 0,
        }
    }
    /// Move the progress bar, without telling the engine. Reports from the
    /// engine are ignored mid-scrub — what we painted is newer than anything
    /// the engine knows yet.
    pub(crate) fn set_local_position(&mut self, position_ms: u32, from_engine: bool) {
        if !should_apply_engine_position(from_engine, self.seek_target) {
            return;
        }
        if let Some(n) = self.now.as_mut() {
            let moved = position_ms != n.position_ms;
            n.position_ms = position_ms.min(n.duration_ms);
            n.position_at = Instant::now();
            if moved {
                self.last_advance = Some(Instant::now());
            }
        }
    }
    /// Seek to an absolute position (clamped), updating the local display too.
    pub(crate) fn seek_to(&mut self, engine: &Engine, position_ms: u32) {
        let Some(dur) = self.now.as_ref().map(|n| n.duration_ms) else {
            return;
        };
        let new = position_ms.min(dur);
        let _ = engine.seek(new);
        self.set_local_position(new, false);
    }
    /// One Shift+arrow press, moving the playhead by `delta_ms`.
    ///
    /// A seek per key repeat overshot the track and restarted the decoder
    /// 30×/s — that pile-up was the stutter. Repeats are throttled and the
    /// engine seek deferred to `flush_seek`.
    pub(crate) fn seek_step(&mut self, delta_ms: i64) {
        let now = Instant::now();
        if self.seek_target.is_some() && now.duration_since(self.seek_last_step) < SEEK_REPEAT {
            // The settle timer must see it, or a long hold commits early.
            self.seek_last_input = now;
            return;
        }
        let Some(dur) = self.now.as_ref().map(|n| n.duration_ms) else {
            return;
        };
        let from = self.seek_target.unwrap_or_else(|| self.position_ms());
        let target = scrub_target(from, dur, delta_ms);
        self.seek_target = Some(target);
        self.seek_last_step = now;
        self.seek_last_input = now;
        self.set_local_position(target, false);
    }
    /// Commit a finished scrub as a single engine seek, once the keys stop.
    pub(crate) fn flush_seek(&mut self, engine: &Engine, now: Instant) {
        if now.duration_since(self.seek_last_input) < SEEK_SETTLE {
            return;
        }
        if let Some(target) = self.seek_target.take() {
            self.seek_to(engine, target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stall_requires_an_advance_then_silence_past_the_grace() {
        let now = Instant::now();
        // No advance yet (fresh track still buffering): never reported.
        assert!(!stall_status(true, None, now, STALL_GRACE));
        // Advance within the grace window: fine.
        assert!(!stall_status(
            true,
            Some(now - Duration::from_secs(3)),
            now,
            STALL_GRACE
        ));
        // Silence past the grace while playing: stalled.
        assert!(stall_status(
            true,
            Some(now - Duration::from_secs(5)),
            now,
            STALL_GRACE
        ));
        // Paused is never a stall.
        assert!(!stall_status(
            false,
            Some(now - Duration::from_secs(120)),
            now,
            STALL_GRACE
        ));
    }
}
