//! Unit tests for [`crate::storage_wakeup`] — which pending operations
//! are worth a frame and which let the loop sleep.
//!
//! Split out of `storage_wakeup.rs` per the 800-line rule (CLAUDE.md) and
//! re-attached via `#[path]`, so `use super::*` reaches its private items.
//!
//! `wants_draw` merges a process-global counter that any other test thread
//! may bump via `signal_async_state_change`, so the "must not want a draw"
//! assertions are written as "at least one of N attempts saw the loop
//! allowed to sleep" — no amount of interference can stop that from
//! happening eventually, while a real per-frame keep-alive would fail
//! every attempt.

use super::*;

use atomartist_storage::{Job, JobCompleter, StorageUri};

use crate::storage_ops::JobOp;

/// Attempts allowed for a "the loop may sleep" observation.
const ATTEMPTS: usize = 5;

fn state() -> AppState {
    AppState::new(
        atomartist_lib::Graph::new(),
        atomartist_lib::registry::NodeRegistry::new(),
    )
}

/// Queue one operation over a job nobody has settled, and hand back the
/// completer so the caller controls when (and with what progress) it
/// settles. `quiet` picks the loud / quiet flavour.
fn queue_pending_op(
    state: &AppState,
    label: &str,
    quiet: bool,
) -> JobCompleter<Option<StorageUri>> {
    let (job, completer) = Job::<Option<StorageUri>>::pending();
    let op = if quiet {
        JobOp::new_quiet(label, job, |_state, _result| {})
    } else {
        JobOp::new(label, job, |_state, _result| {})
    };
    state.submit_op(Box::new(op));
    completer
}

/// Run `pump_storage` up to [`ATTEMPTS`] times from a cleared draw flag
/// and report whether any of those frames left the loop free to sleep.
fn saw_an_idle_frame(state: &AppState) -> bool {
    for _ in 0..ATTEMPTS {
        agg_gui::animation::clear_draw_request();
        assert!(state.pump_storage(), "the operation is still in flight");
        if !agg_gui::animation::wants_draw() {
            return true;
        }
    }
    false
}

/// The step-6g-1 deliverable: an operation that is merely *waiting* — the
/// file picker, sitting on screen until the user answers — must not ask
/// for a frame. Its result arrives by signal (the host waker) or by the
/// event that settles it, not by polling.
#[test]
fn a_pending_op_with_no_progress_lets_the_loop_sleep() {
    let state = state();
    let completer = queue_pending_op(&state, "Choose a file to open", false);
    assert!(
        saw_an_idle_frame(&state),
        "a pending op with nothing to animate must not keep requesting frames"
    );
    drop(completer);
    state.pump_storage();
}

/// …and the status bar agrees, because both read the same predicate. A
/// widget that reported `needs_draw` here would pin the loop just as
/// effectively as the old keep-alive did.
#[test]
fn a_pending_op_with_no_progress_does_not_animate_the_status_bar() {
    let state = state();
    let completer = queue_pending_op(&state, "Choose a file to open", false);
    assert_eq!(state.pending_op_count(), 1, "it really is in flight");
    assert!(
        !state.has_progress_reporting_op(),
        "nothing about it changes between frames"
    );
    drop(completer);
    state.pump_storage();
}

/// Progress is polled, not signalled, so an operation that reports a
/// percentage still earns a frame per frame — that is what advances the
/// status bar's readout.
#[test]
fn a_progress_reporting_op_still_keeps_the_loop_alive() {
    let state = state();
    let completer = queue_pending_op(&state, "Opening bracket.atmr", false);
    completer.set_progress(Some(0.42));

    assert!(state.has_progress_reporting_op());
    for _ in 0..ATTEMPTS {
        agg_gui::animation::clear_draw_request();
        assert!(state.pump_storage(), "the operation is still in flight");
        assert!(
            agg_gui::animation::wants_draw(),
            "an advancing percentage must bring the loop back next frame"
        );
    }
    drop(completer);
    state.pump_storage();
}

/// Quiet background work has no readout, so even *its* progress is not
/// worth a frame: nothing on screen would change.
#[test]
fn a_quiet_progress_reporting_op_does_not_keep_the_loop_alive() {
    let state = state();
    let completer = queue_pending_op(&state, "Preview a.atmr", true);
    completer.set_progress(Some(0.42));

    assert!(
        !state.has_progress_reporting_op(),
        "quiet work is not in the readout, so it animates nothing"
    );
    assert!(
        saw_an_idle_frame(&state),
        "background progress must not pin the frame loop"
    );
    drop(completer);
    state.pump_storage();
}

/// What earns the right to let the loop sleep: a job settled by a *worker
/// thread* must wake the host by itself, through the storage completion
/// hook every shell installs ([`crate::shell_init::install_storage_wakeups`]).
///
/// Without this the narrowing above would be a silent-data-loss bug —
/// nothing else would ever look at that job again.
///
/// The hook is a process-global that other tests in this binary may also
/// install, so the assertion is on the *draw signal*, which any correct
/// hook raises, rather than on our own closure being the one that ran.
#[test]
fn a_worker_thread_settle_wakes_the_host() {
    crate::shell_init::install_storage_wakeups();
    let state = state();
    let completer = queue_pending_op(&state, "Opening a.bin", false);

    agg_gui::animation::clear_draw_request();
    assert!(state.pump_storage(), "still in flight, and no frame owed");

    // Settle it from another thread — the case a parked event loop cannot
    // discover on its own.
    let worker = std::thread::spawn(move || completer.succeed(None));
    worker.join().expect("worker finishes");

    assert!(
        agg_gui::animation::wants_draw(),
        "an off-thread completion must raise the draw signal that wakes the host"
    );
    assert!(
        !state.pump_storage(),
        "and the very next pump applies and drains it"
    );
}

/// Nothing queued means nothing scheduled — an idle app must go all the
/// way to sleep.
#[test]
fn an_empty_queue_arms_nothing() {
    let state = state();
    let mut saw_idle = false;
    for _ in 0..ATTEMPTS {
        agg_gui::animation::clear_draw_request();
        assert!(!state.pump_storage());
        if !agg_gui::animation::wants_draw()
            && agg_gui::animation::peek_next_draw_deadline().is_none()
        {
            saw_idle = true;
            break;
        }
    }
    assert!(saw_idle, "an empty queue must not schedule any wake-up");
}
