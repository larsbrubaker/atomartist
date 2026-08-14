//! Frame-loop storage pump, driven through the real widget-tree harness.
//!
//! No NodeDesigner counterpart — this covers AtomArtist's own async
//! storage seam (`docs/storage-architecture-plan.md` §3.3, phase 4). The
//! unit tests in `atomartist-ui/src/storage_ops.rs` cover the pump's
//! semantics; these check that a harness-hosted `AppState` — the same one
//! the widgets hold — pumps from `TestHarness::pump` /
//! `pump_until_idle`, exactly as the shells do once per frame.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use atomartist_storage::{
    FlakyConfig, FlakyProvider, MemoryProvider, Precondition, StorageProvider, StorageUri,
};
use atomartist_ui::storage_ops::{JobOp, NoticeLevel};
use atomartist_ui_test::TestHarness;

/// A memory-backed provider with `latency` ticks of simulated delay, plus
/// its root URI.
fn slow_provider(latency: usize) -> (Arc<FlakyProvider>, StorageUri) {
    let inner = MemoryProvider::new("mem", "Memory");
    let root = inner.root();
    let config = FlakyConfig::default().with_latency(latency);
    (Arc::new(FlakyProvider::new(Arc::new(inner), config)), root)
}

#[test]
fn harness_pump_applies_a_delayed_op() {
    let h = TestHarness::new();
    let (provider, root) = slow_provider(2);
    let at = root.join("bracket.atmr");

    let runs = Arc::new(AtomicUsize::new(0));
    let counter = runs.clone();
    h.state().submit_op(Box::new(JobOp::new(
        "Saving bracket.atmr",
        provider.write(&at, b"bytes".to_vec(), Precondition::None),
        move |state, result| {
            result.expect("write succeeds");
            counter.fetch_add(1, Ordering::Relaxed);
            state.notify(NoticeLevel::Info, "Saved bracket.atmr");
        },
    )));

    assert_eq!(h.state().pending_op_count(), 1);
    assert_eq!(
        h.state().pending_op_status(),
        vec![("Saving bracket.atmr".to_string(), None)]
    );

    // Frame 1: the provider's clock has not delivered the result yet.
    provider.pump();
    assert!(h.pump(), "still in flight");
    assert_eq!(runs.load(Ordering::Relaxed), 0);

    // Frame 2: the result lands, the continuation runs, the queue drains.
    provider.pump();
    assert!(!h.pump());
    assert_eq!(runs.load(Ordering::Relaxed), 1);
    assert_eq!(h.state().pending_op_count(), 0);

    let notices = h.state().drain_notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].level, NoticeLevel::Info);
    assert_eq!(notices[0].text, "Saved bracket.atmr");
}

/// `pump_until_idle` must terminate on an already-empty queue, and must
/// not need the caller to count frames when the work is done.
#[test]
fn pump_until_idle_returns_immediately_when_nothing_is_queued() {
    let h = TestHarness::new();
    h.pump_until_idle(4);
    assert_eq!(h.state().pending_op_count(), 0);
}

/// A test must fail rather than hang when an operation never settles.
#[test]
#[should_panic(expected = "Opening stuck.atmr")]
fn pump_until_idle_panics_naming_the_stuck_op() {
    let h = TestHarness::new();
    let (provider, root) = slow_provider(1_000);
    let at = root.join("stuck.atmr");
    h.state().submit_op(Box::new(JobOp::new(
        "Opening stuck.atmr",
        provider.read(&at),
        |_state, _result| {},
    )));
    h.pump_until_idle(3);
}
