//! The status bar's storage segment — in-flight readout, the cancel
//! affordance, and the sticky notice.
//!
//! No NodeDesigner counterpart: this is AtomArtist's own async storage
//! seam (`docs/storage-architecture-plan.md` §3.3, phase 4b). The unit
//! tests in `atomartist-ui/src/storage_ops_tests.rs` cover the strings and
//! the queue; these drive the *real* `StatusBar` widget in the production
//! tree, so a click lands through agg-gui's hit-testing exactly as it does
//! on the desktop.

use std::sync::{Arc, Mutex};

use agg_gui::{MouseButton, Size};
use atomartist_storage::{
    FlakyConfig, FlakyProvider, MemoryProvider, StorageError, StorageProvider, StorageUri,
};
use atomartist_ui::storage_ops::{JobOp, NoticeLevel};
use atomartist_ui_test::harness::{DEFAULT_HEIGHT, DEFAULT_WIDTH};
use atomartist_ui_test::TestHarness;

/// A memory-backed provider with `latency` ticks of simulated delay (so an
/// operation is genuinely in flight while the test clicks), plus its root.
fn flaky(latency: usize) -> (Arc<FlakyProvider>, StorageUri) {
    let inner = MemoryProvider::new("mem", "Memory");
    let root = inner.root();
    (
        Arc::new(FlakyProvider::new(
            Arc::new(inner),
            FlakyConfig::default().with_latency(latency),
        )),
        root,
    )
}

/// The `(name, value)` pairs the live `StatusBar` publishes, read back
/// through agg-gui's reflection walk — the same data the inspector shows.
fn status_bar_props(h: &TestHarness) -> Vec<(&'static str, String)> {
    h.snapshot()
        .into_iter()
        .find(|n| n.type_name == "StatusBar")
        .expect("the status bar is in the widget tree")
        .properties
}

fn prop(h: &TestHarness, name: &str) -> String {
    status_bar_props(h)
        .into_iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| v)
        .unwrap_or_default()
}

/// Screen-space (Y-down) point of the status bar's cancel affordance.
fn cancel_point(h: &TestHarness) -> (f64, f64) {
    let node = h
        .snapshot()
        .into_iter()
        .find(|n| n.type_name == "StatusBar")
        .expect("the status bar is in the widget tree");
    let local_x: f64 = node
        .properties
        .iter()
        .find(|(n, _)| *n == "cancel_center_x")
        .map(|(_, v)| v.clone())
        .filter(|v| !v.is_empty())
        .expect("cancel affordance is present while an op is in flight")
        .parse()
        .expect("cancel_center_x is a number");
    let b = node.screen_bounds;
    // `screen_bounds` is Y-up; the harness's click helpers take the
    // shell's Y-down physical coordinates.
    (b.x + local_x, DEFAULT_HEIGHT - (b.y + b.height * 0.5))
}

fn relayout(h: &mut TestHarness) {
    h.app_mut().layout(Size::new(DEFAULT_WIDTH, DEFAULT_HEIGHT));
}

#[test]
fn the_status_bar_shows_the_pending_ops_label() {
    let mut h = TestHarness::new();
    relayout(&mut h);
    assert_eq!(prop(&h, "storage"), "", "idle reserves no readout");

    let (provider, root) = flaky(5);
    h.state().submit_op(Box::new(JobOp::new(
        "Opening bracket.atmr",
        provider.read(&root.join("bracket.atmr")),
        |_state, _result| {},
    )));
    relayout(&mut h);

    assert_eq!(prop(&h, "storage"), "Opening bracket.atmr…");
    assert!(
        !prop(&h, "cancel_center_x").is_empty(),
        "a pending op gets a cancel affordance"
    );

    // Draining it takes the segment away again.
    provider.pump_until_idle();
    h.pump();
    relayout(&mut h);
    assert_eq!(prop(&h, "storage"), "");
    assert_eq!(prop(&h, "cancel_center_x"), "");
}

#[test]
fn a_second_pending_op_is_counted_in_the_readout() {
    let mut h = TestHarness::new();
    let (provider, root) = flaky(5);
    for name in ["a.atmr", "b.atmr"] {
        h.state().submit_op(Box::new(JobOp::new(
            format!("Opening {name}"),
            provider.read(&root.join(name)),
            |_state, _result| {},
        )));
    }
    relayout(&mut h);
    assert_eq!(prop(&h, "storage"), "Opening a.atmr… (+1 more)");
}

#[test]
fn clicking_the_cancel_affordance_cancels_the_pending_ops() {
    let mut h = TestHarness::new();
    let (provider, root) = flaky(5);

    let seen: Arc<Mutex<Option<StorageError>>> = Arc::new(Mutex::new(None));
    let sink = seen.clone();
    h.state().submit_op(Box::new(JobOp::new(
        "Opening bracket.atmr",
        provider.read(&root.join("bracket.atmr")),
        move |_state, result| {
            *sink.lock().unwrap() = result.err();
        },
    )));
    relayout(&mut h);
    assert_eq!(h.state().pending_op_count(), 1, "the op is really pending");

    let (x, y) = cancel_point(&h);
    h.click(x, y, MouseButton::Left);

    // Cancellation settles the job; the next pump applies the
    // continuation with `Cancelled`, exactly as any other outcome.
    h.pump();
    assert_eq!(*seen.lock().unwrap(), Some(StorageError::Cancelled));
    assert_eq!(h.state().pending_op_count(), 0);
}

/// A click that lands on the bar but outside the storage affordances must
/// pass through — the status bar is informational everywhere else.
#[test]
fn clicking_elsewhere_on_the_bar_leaves_pending_ops_alone() {
    let mut h = TestHarness::new();
    let (provider, root) = flaky(5);
    h.state().submit_op(Box::new(JobOp::new(
        "Opening bracket.atmr",
        provider.read(&root.join("bracket.atmr")),
        |_state, _result| {},
    )));
    relayout(&mut h);

    let (_, y) = cancel_point(&h);
    h.click(4.0, y, MouseButton::Left);
    h.pump();
    assert_eq!(
        h.state().pending_op_count(),
        1,
        "a click on the zoom readout must not cancel anything"
    );
}

#[test]
fn a_notice_reaches_the_status_bar_and_a_click_dismisses_it() {
    let mut h = TestHarness::new();
    h.state()
        .notify(NoticeLevel::Error, "could not open bracket.atmr");
    // The shells' once-per-frame pump is what moves a notice onto the bar.
    h.pump();
    relayout(&mut h);
    assert_eq!(prop(&h, "notice"), "could not open bracket.atmr");

    // The notice sits where the (absent) activity readout would start, so
    // its span begins at the segment's left edge.
    let node = h
        .snapshot()
        .into_iter()
        .find(|n| n.type_name == "StatusBar")
        .expect("status bar");
    let b = node.screen_bounds;
    let x = b.x + atomartist_ui::status_bar::STORAGE_X + 4.0;
    let y = DEFAULT_HEIGHT - (b.y + b.height * 0.5);
    h.click(x, y, MouseButton::Left);

    assert_eq!(h.state().last_notice(), None, "clicking dismisses it");
    relayout(&mut h);
    assert_eq!(prop(&h, "notice"), "");
}
