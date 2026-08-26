//! Frame-rate / wakeup measurement for the native shell
//! (`ATOMARTIST_FPS_LOG=1`).
//!
//! Everything else the old `wake` module carried — the host waker over a
//! winit proxy and the scheduled-deadline merge — is owned by
//! `agg-gui-shell` now; this probe is what remains AtomArtist's. It
//! measures painted frames and loop wake-ups per second, which is how the
//! "idle dialog pins the framerate" claim was checked against the real
//! shell rather than guessed at (CLAUDE.md: never guess at performance).

use std::time::{Duration, Instant};

/// Frames painted and loop wake-ups served, reported once a second when
/// `ATOMARTIST_FPS_LOG=1`.
///
/// Deliberately opt-in and side-effect-free when off: this exists to
/// *measure* the idle behaviour, not to add a permanent per-frame cost.
pub struct FrameRateProbe {
    enabled: bool,
    window_start: Instant,
    frames: u32,
    wakeups: u32,
}

impl FrameRateProbe {
    pub fn new() -> Self {
        FrameRateProbe {
            enabled: std::env::var("ATOMARTIST_FPS_LOG").is_ok_and(|v| v != "0"),
            window_start: Instant::now(),
            frames: 0,
            wakeups: 0,
        }
    }

    /// One painted frame.
    pub fn frame(&mut self) {
        if self.enabled {
            self.frames = self.frames.saturating_add(1);
        }
    }

    /// One idle turn of the loop (an `AboutToWait`), painted or not.
    ///
    /// `pending` reports how many storage operations were queued at that
    /// moment — the number that used to guarantee the frame counter kept
    /// climbing. It is a closure because answering it locks the pending-op
    /// queue, and this probe must cost nothing at all when logging is off.
    ///
    /// Reporting is driven from here, so a genuinely parked loop prints
    /// nothing at all: silence for N seconds *is* the "zero frames"
    /// reading, and it is the reading a healthy idle dialog produces.
    pub fn wakeup(&mut self, pending: impl FnOnce() -> usize) {
        if !self.enabled {
            return;
        }
        self.wakeups = self.wakeups.saturating_add(1);
        let elapsed = self.window_start.elapsed();
        if elapsed < Duration::from_secs(1) {
            return;
        }
        let secs = elapsed.as_secs_f64();
        eprintln!(
            "fps: {:.1} painted/s, {:.1} wakeups/s, {} storage op(s) pending",
            self.frames as f64 / secs,
            self.wakeups as f64 / secs,
            pending(),
        );
        self.window_start = Instant::now();
        self.frames = 0;
        self.wakeups = 0;
    }
}

impl Default for FrameRateProbe {
    fn default() -> Self {
        Self::new()
    }
}
