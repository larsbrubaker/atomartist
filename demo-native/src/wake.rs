//! Waking the native event loop from off the main thread, and measuring
//! how often it wakes at all (step 6g-1 of `docs/file-browser-design.md`).
//!
//! `demo-native` is a *reactive* host: it parks in `ControlFlow::Wait` and
//! only runs when winit hands it something. That is what makes an idle
//! AtomArtist cost nothing, and it is also why a worker thread finishing a
//! storage job used to be invisible — `signal_async_state_change` bumps a
//! process-global counter the main thread merges on its next
//! `wants_draw()` read, and a parked loop performs no reads at all.
//! `atomartist-ui`'s pump papered over that by requesting a frame every
//! frame while anything was queued, which pinned the app at full framerate
//! for as long as a file dialog sat open.
//!
//! agg-gui 851bff0 closes the gap with an optional host waker
//! (`agg_gui::animation::set_host_waker`): a cheap, thread-safe nudge that
//! `signal_async_state_change` calls *after* publishing the counter. Ours
//! is an [`EventLoopProxy::send_event`], so the loop wakes, delivers
//! `Event::UserEvent`, and reaches its `AboutToWait` pump with the new
//! counter value already visible. See `atomartist_ui::storage_wakeup` for
//! what the pump then does (and does not) ask for.
//!
//! [`FrameRateProbe`] is the measurement half: `ATOMARTIST_FPS_LOG=1`
//! prints painted frames and loop wake-ups per second, which is how the
//! "idle dialog pins the framerate" claim was checked against the real
//! shell rather than guessed at.

use std::time::{Duration, Instant};

use winit::event_loop::EventLoopProxy;

/// Install the process-global host waker over this loop's proxy.
///
/// Failure-tolerant per the agg-gui contract: once the loop has exited,
/// `send_event` returns `Err` and there is nothing to wake, so the error
/// is dropped rather than panicking on a background thread.
pub fn install_host_waker(proxy: EventLoopProxy<()>) {
    agg_gui::animation::set_host_waker(move || {
        let _ = proxy.send_event(());
    });
}

/// Drop the waker — and with it the retained proxy — on the way out.
///
/// Called on every path that leaves the event loop, so a late worker
/// thread signalling into a dead loop is a no-op instead of a queued
/// event nobody will ever read.
pub fn clear_host_waker() {
    agg_gui::animation::clear_host_waker();
}

/// What the loop should do when nothing wants a frame right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextTurn {
    /// A scheduled draw has come due — redraw immediately.
    Now,
    /// Sleep, but no later than this instant.
    Until(Instant),
    /// Sleep until an event, a waker nudge, or a signal arrives.
    Indefinitely,
}

/// Decide the loop's next turn from the two scheduled-draw sources.
///
/// `held` is the deadline this loop is already carrying (armed during a
/// paint); agg-gui's `peek_next_draw_deadline` is anything armed since —
/// a `request_draw_after` from an event handler or the storage pump, i.e.
/// *outside* a paint, which the `RedrawRequested` arm never sees.
///
/// They are merged by **earliest**, not chained: preferring whichever
/// source is checked first lets a stale, later deadline shadow a sooner
/// one that was just armed, and the sooner wake-up then arrives late (or,
/// if the held one is cleared first, not at all).
pub fn next_turn(held: Option<Instant>) -> NextTurn {
    let now = Instant::now();
    // agg-gui deals in `web_time::Instant`; convert through a remaining
    // duration rather than assuming the two clocks share an epoch.
    let armed = agg_gui::animation::peek_next_draw_deadline()
        .map(|deadline| now + deadline.saturating_duration_since(web_time::Instant::now()));
    let soonest = match (held, armed) {
        (Some(held), Some(armed)) => Some(held.min(armed)),
        (held, armed) => held.or(armed),
    };
    match soonest {
        Some(when) if now >= when => NextTurn::Now,
        Some(when) => NextTurn::Until(when),
        None => NextTurn::Indefinitely,
    }
}

/// Frames painted and loop wake-ups served, reported once a second when
/// `ATOMARTIST_FPS_LOG=1`.
///
/// Deliberately opt-in and side-effect-free when off: this exists to
/// *measure* the idle behaviour (CLAUDE.md: never guess at performance),
/// not to add a permanent per-frame cost.
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

    /// One turn of the loop (an `AboutToWait`), painted or not.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The merge that item 1 of the 6g-1 review asked for: a deadline
    /// armed since the last paint must not be shadowed by a later one the
    /// loop happens to be holding.
    #[test]
    fn a_sooner_armed_deadline_wins_over_a_later_held_one() {
        agg_gui::animation::clear_draw_request();
        agg_gui::animation::request_draw_after(Duration::from_millis(50));
        let held = Instant::now() + Duration::from_secs(60);

        match next_turn(Some(held)) {
            NextTurn::Until(when) => assert!(
                when < held,
                "the freshly armed 50 ms deadline must win over the held 60 s one"
            ),
            other => panic!("expected a scheduled sleep, got {other:?}"),
        }
        agg_gui::animation::clear_draw_request();
    }

    /// …and symmetrically, a held deadline sooner than anything armed is
    /// the one that survives.
    #[test]
    fn a_sooner_held_deadline_wins_over_a_later_armed_one() {
        agg_gui::animation::clear_draw_request();
        agg_gui::animation::request_draw_after(Duration::from_secs(60));
        let held = Instant::now() + Duration::from_millis(50);

        match next_turn(Some(held)) {
            NextTurn::Until(when) => assert_eq!(when, held),
            other => panic!("expected a scheduled sleep, got {other:?}"),
        }
        agg_gui::animation::clear_draw_request();
    }

    /// A deadline already in the past is a draw, not a sleep.
    #[test]
    fn a_due_deadline_asks_for_a_frame() {
        agg_gui::animation::clear_draw_request();
        let held = Instant::now() - Duration::from_millis(1);
        assert_eq!(next_turn(Some(held)), NextTurn::Now);
    }

    /// Nothing scheduled anywhere: the loop is allowed to sleep until
    /// something wakes it. This is the state an idle app — including one
    /// showing an open file dialog — must reach after step 6g-1.
    #[test]
    fn nothing_scheduled_sleeps_indefinitely() {
        agg_gui::animation::clear_draw_request();
        assert_eq!(next_turn(None), NextTurn::Indefinitely);
    }
}
