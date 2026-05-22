//! Tests for the `View → Debug` floating windows wiring.
//!
//! These verify the menu action callbacks land on the shared
//! visibility cells, the inspector queues and frame-history handles
//! survive a roundtrip through `build_app`, and the debug windows
//! show up inside the production widget tree.

use agg_gui::RunMode;
use atomartist_ui::DebugWindowsState;
use atomartist_ui_test::TestHarness;

#[test]
fn debug_windows_default_hidden() {
    let h = TestHarness::new();
    assert!(!h.debug().inspector_visible.get(), "inspector starts hidden");
    assert!(!h.debug().perf_visible.get(), "performance starts hidden");
}

#[test]
fn build_app_mounts_inspector_and_performance_windows_in_tree() {
    // The production tree builds an InspectorPanel + PerformanceView
    // even when the windows are hidden — visibility is just whether
    // the wrapping Window draws/hit-tests them. If reflection can't
    // find them, the toggle path will be a no-op.
    let h = TestHarness::new();
    assert!(
        h.find_by_type("InspectorPanel").is_some(),
        "Inspector widget should live in the tree"
    );
    assert!(
        h.find_by_type("PerformanceView").is_some(),
        "Performance widget should live in the tree"
    );
}

#[test]
fn toggling_visibility_cells_directly_round_trips() {
    // We don't hit-test menu-bar pixels here — the menu's action
    // callback writes the same `Rc<Cell<bool>>` we hold a clone of,
    // so flipping the cell is the moral equivalent of clicking
    // `View → Debug → Inspector`. The harness exposes the handle
    // exactly so tests can short-circuit hit-testing.
    let h = TestHarness::new();
    let cell = h.debug().inspector_visible.clone();
    assert!(!cell.get());
    cell.set(true);
    // Production handlers read the same Rc, so the menu would
    // observe the flip on its next click.
    assert!(h.debug().inspector_visible.get());
}

#[test]
fn frame_history_is_writable_and_observed_by_perf_widget() {
    // The shell pushes wall-clock frame samples into the same
    // `SharedFrameHistory` `PerformanceView` reads from. Pushing a
    // sample must update the mean immediately.
    let h = TestHarness::new();
    let history = h.debug().frame_history.clone();
    {
        let mut hist = history.borrow_mut();
        hist.push(8.0);
        hist.push(12.0);
    }
    let mean = history.borrow().mean_ms();
    assert!((mean - 10.0).abs() < 1e-6, "mean of [8, 12] should be 10.0, got {mean}");
}

#[test]
fn perf_widget_never_claims_its_own_redraws() {
    // Regression: pushing a sample used to flip
    // `PerformanceView::needs_draw()` to true via the
    // `with_history_redraw(true)` opt-in, which then trapped the
    // shell into a continuous repaint loop (paint → push sample →
    // needs_draw=true → paint → …).  AtomArtist now drops the
    // history-redraw opt-in entirely and relies on the Window's
    // `with_live_content(true)` to refresh the cached pixels
    // whenever an external invalidation triggers a paint.  Net
    // effect: the perf widget never claims its own redraws in
    // either mode — Continuous-mode pumping is driven by the
    // native shell's main loop reading `run_mode`, not by any
    // widget claim.
    let h = TestHarness::new();
    h.debug().perf_visible.set(true);

    {
        let mut hist = h.debug().frame_history.borrow_mut();
        hist.push(8.0);
        hist.push(12.0);
    }

    // Reactive — the original (and primary) regression.
    assert_eq!(h.debug().run_mode.get(), RunMode::Reactive);
    let perf = h
        .find_by_type("PerformanceView")
        .expect("Performance widget should live in the tree");
    assert!(
        !perf.needs_draw(),
        "Reactive mode must NOT claim redraws even with fresh history samples"
    );

    // Continuous — symmetry check.  The shell loop is responsible
    // for pumping in Continuous; the widget itself stays passive.
    h.debug().run_mode.set(RunMode::Continuous);
    let perf = h
        .find_by_type("PerformanceView")
        .expect("Performance widget should live in the tree");
    assert!(
        !perf.needs_draw(),
        "Continuous-mode redraws come from the shell loop, not from the perf widget"
    );
}

#[test]
fn run_mode_defaults_to_reactive_and_round_trips_through_cell() {
    // AtomArtist should start in Reactive (only paint on input or
    // animation), unlike Antidote / sim-driven apps that pump frames
    // continuously.  The Reactive/Continuous segmented buttons inside
    // the Performance window's `PerformanceView` write through to the
    // same `run_mode` cell the native shell's main loop reads, so a
    // direct cell flip is the moral equivalent of clicking a button.
    let h = TestHarness::new();
    let cell = h.debug().run_mode.clone();
    assert_eq!(cell.get(), RunMode::Reactive, "default starts Reactive");

    cell.set(RunMode::Continuous);
    assert_eq!(h.debug().run_mode.get(), RunMode::Continuous);

    // The tree must contain a RunModeRow so the user has a way to
    // flip back without re-launching the app.
    assert!(
        h.find_by_type("RunModeRow").is_some(),
        "RunModeRow segmented buttons should live inside PerformanceView"
    );
}

#[test]
fn saved_layout_seeds_the_handle_cells() {
    // Tests using a persisted layout build `UiSettings` manually
    // and feed it to `build_app`. We exercise that path through the
    // smaller surface of `DebugWindowHandles::new` — the harness's
    // default is None.
    use atomartist_ui::{DebugWindowHandles, DebugWindowState};
    let saved = DebugWindowsState {
        inspector: DebugWindowState {
            open: true,
            x: 200.0,
            y: 250.0,
            width: 480.0,
            height: 600.0,
        },
        performance: DebugWindowState {
            open: true,
            x: 800.0,
            y: 100.0,
            width: 360.0,
            height: 180.0,
        },
    };
    let h = DebugWindowHandles::new(saved);
    assert!(h.inspector_visible.get());
    assert!(h.perf_visible.get());
    assert_eq!(h.inspector_bounds.get().x, 200.0);
    assert_eq!(h.inspector_bounds.get().width, 480.0);
    assert_eq!(h.perf_bounds.get().x, 800.0);
    // Round-trip through current_state preserves what we set.
    assert_eq!(h.current_state(), saved);
}
