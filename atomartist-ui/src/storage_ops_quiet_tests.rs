//! Unit tests for the *quiet* half of [`crate::storage_ops`]: background
//! operations that stay out of the status bar, out of the File menu's
//! busy check, and out of the shutdown wait.
//!
//! A file of its own because `storage_ops_tests.rs` is at the 800-line
//! cap (CLAUDE.md); attached from the same parent with `#[path]`, so
//! `use super::*` reaches the module's private items and
//! `super::storage_ops_tests` its shared fixtures.

use super::storage_ops_tests::{flaky, state};
use super::*;
use atomartist_storage::{FlakyConfig, StorageProvider};
use std::sync::atomic::{AtomicUsize, Ordering};

/// (a) Background work stays out of the user's way: no activity readout,
/// no "storage is busy", no place in the status bar's backlog count — but
/// it is still on the queue and still applied.
#[test]
fn a_quiet_op_is_invisible_to_the_status_bar_and_the_busy_check() {
    let state = state();
    let (provider, root) = flaky(FlakyConfig::default().with_latency(5));

    let applied = Arc::new(AtomicUsize::new(0));
    let counter = applied.clone();
    state.submit_op(Box::new(JobOp::new_quiet(
        "Preview bracket.atmr",
        provider.read(&root.join("bracket.atmr")),
        move |_state, _result| {
            counter.fetch_add(1, Ordering::SeqCst);
        },
    )));
    assert_eq!(state.pending_op_count_all(), 1, "it is really queued");
    assert_eq!(state.pending_op_count(), 0, "but not as user-visible work");
    assert_eq!(state.storage_activity_text(), None);
    assert!(state.pending_op_status().is_empty());

    // A loud op alongside it leads the readout and counts alone — the
    // preview must not show up as "(+1 more)".
    state.submit_op(Box::new(JobOp::new(
        "Opening bracket.atmr",
        provider.read(&root.join("bracket.atmr")),
        |_state, _result| {},
    )));
    assert_eq!(state.pending_op_count(), 1);
    assert_eq!(state.pending_op_count_all(), 2);
    assert_eq!(
        state.storage_activity_text().as_deref(),
        Some("Opening bracket.atmr…"),
        "a quiet op must not be counted in the backlog"
    );

    // Quiet only changes visibility: the op is pumped and applied as usual.
    for _ in 0..8 {
        provider.pump();
        state.pump_storage();
    }
    assert_eq!(applied.load(Ordering::SeqCst), 1);
    assert_eq!(state.pending_op_count_all(), 0);
}

/// (b) The shutdown drain does not wait for background work. A thumbnail
/// read on a provider that never settles must not hold the window open —
/// it is cancelled, its continuation observes `Cancelled`, and the close
/// reports success because nothing the user asked for was lost.
#[test]
fn draining_does_not_wait_for_quiet_ops() {
    let state = state();
    // Latency far beyond any deadline, and nothing pumps the provider.
    let (provider, root) = flaky(FlakyConfig::default().with_latency(1_000_000));

    let seen = Arc::new(Mutex::new(None::<StorageError>));
    let sink = seen.clone();
    state.submit_op(Box::new(JobOp::new_quiet(
        "Preview bracket.atmr",
        provider.read(&root.join("bracket.atmr")),
        move |_state, result| {
            *lock(&sink) = result.err();
        },
    )));

    let started = std::time::Instant::now();
    let drained = state.drain_pending_ops(std::time::Duration::from_secs(30));
    assert!(drained, "a stuck preview must not fail the close");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "the drain must not have waited out the timeout"
    );
    assert_eq!(
        *lock(&seen),
        Some(StorageError::Cancelled),
        "the continuation still observes an outcome"
    );
    assert_eq!(state.pending_op_count_all(), 0, "nothing is left behind");
}

