//! When the frame loop is allowed to sleep while storage work is queued —
//! the wake-up half of [`crate::storage_ops`] (step 6g-1 of
//! `docs/file-browser-design.md` §5c).
//!
//! [`AppState::pump_storage`](crate::AppState::pump_storage) has to leave
//! the host in a state where the *next* interesting moment is still
//! reached. Through step 6f it did that the blunt way: while anything was
//! queued it asked for another frame, every frame. That is correct and
//! ruinous — the file-picker modal's `JobOp` stays pending for as long as
//! the user takes to answer, so an idle open dialog pinned the app at full
//! framerate with a completely static window on screen (measured at
//! 120 / 120 idle loop turns painting in
//! `atomartist-ui-test/tests/idle_wakeups.rs`).
//!
//! # Every way a queued operation gets looked at again
//!
//! - **A worker thread finishes something** and calls
//!   `agg_gui::animation::signal_async_state_change`. Since agg-gui
//!   851bff0 that also fires the host waker the shell installed
//!   (`demo-native::wake`), so a loop parked in `ControlFlow::Wait` wakes,
//!   pumps, and applies the continuation. This is the mechanism that
//!   replaced the keep-alive.
//! - **A job that was already settled at submit time** never reaches the
//!   queue at all: `submit_op` applies it inline (every local provider).
//! - **The user answers the modal picker.** OK / Cancel / Escape settle
//!   the job from a widget event, which requests a draw on its own; the
//!   shell pumps before the paint that follows.
//! - **A job settles anywhere at all** — including on a worker thread —
//!   and `atomartist_storage`'s completion hook fires. Every shell
//!   installs `signal_async_state_change` into it
//!   ([`crate::shell_init::install_storage_wakeups`]), so the two bullets
//!   above are really one mechanism: settle → signal → waker → frame →
//!   pump.
//! - **A provider reports progress** via
//!   [`atomartist_storage::JobCompleter::set_progress`]. That is *polled*,
//!   not signalled — the status bar's percentage only changes when
//!   somebody looks, and signalling per progress tick would be a wakeup
//!   storm. So an operation that reports fractional progress (and is loud
//!   enough to be on screen) still earns a frame per frame:
//!   [`AppState::has_progress_reporting_op`].
//! - **Nothing at all.** A pending operation with no progress to show owes
//!   no frame and gets none. The loop sleeps until something above
//!   happens — which is the whole point of the step.
//!
//! There is deliberately **no** fallback poll. The interim version of this
//! module ran a 250 ms re-check, because `JobCompleter` settled silently
//! and an unsignalled worker completion would otherwise be observed never;
//! the completion hook removed the need, and with it the ~4 frames/second
//! an open dialog still cost.

use crate::app_state::AppState;

impl AppState {
    /// Whether some *loud* operation is reporting fractional progress —
    /// the one piece of storage state that changes between frames without
    /// anybody signalling it, and the only one the status bar animates.
    ///
    /// Quiet work is excluded for the same reason it is excluded from the
    /// activity readout: nothing paints it, so nothing needs the frame.
    ///
    /// A *settled* loud operation counts too (it polls as `Some(1.0)`),
    /// which is harmless: the frame it earns is the frame that applies it.
    ///
    /// Allocation-free on purpose — the status bar asks this from
    /// `needs_draw`, which the paint walk calls repeatedly per frame, so
    /// the obvious `pending_op_status()` route would clone a `String` per
    /// queued operation each time.
    pub fn has_progress_reporting_op(&self) -> bool {
        self.pending_ops
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|op| {
                !op.is_quiet()
                    && match op.poll() {
                        atomartist_storage::JobState::Pending { progress } => progress.is_some(),
                        _settled => true,
                    }
            })
    }

    /// Ask the host to come back at the right time for whatever is still
    /// queued — the pump's "nothing settled this frame" branch.
    ///
    /// Called only when at least one operation is still pending. See the
    /// module docs for the full list of wake sources; everything not
    /// listed here is somebody else's signal to send.
    pub(crate) fn request_pending_wakeup(&self) {
        if self.has_progress_reporting_op() {
            // Pure keep-alive for an advancing percentage: another frame,
            // no epoch bump (the status bar's `needs_draw` is what
            // re-rasters the strip).
            agg_gui::animation::request_draw_without_invalidation();
        }
    }
}

#[cfg(test)]
#[path = "storage_wakeup_tests.rs"]
mod storage_wakeup_tests;
