//! Regression tests for the WASM shell's blank-page-until-resize bug.
//!
//! Not a port of a NodeDesigner test — this reproduces an AtomArtist-only
//! defect: on the web build the page loaded blank after a refresh (always
//! on mobile) and only drew once the window was resized.
//!
//! Root cause: the app defaults to `RunMode::Reactive`, and `demo-wasm`'s
//! `render()` skipped the whole acquire/layout/paint unless its reactive
//! predicate (`animation::wants_draw() || app.wants_draw() || a due
//! deadline`) was true. Nothing guarantees a draw request is still
//! pending when async wgpu init resolves, so the first frame could be
//! skipped forever. Resizing worked because that path bypasses the gate.
//!
//! `demo-wasm/src/lib.rs` is `#![cfg(target_arch = "wasm32")]`, so the
//! decision itself lives in `atomartist_ui::FirstPaintGate` where a
//! native test can reach the production code the shell actually calls.

use agg_gui::animation;
use atomartist_ui::FirstPaintGate;
use atomartist_ui_test::TestHarness;

/// The bug, stated as an invariant: a freshly built app tree plus a
/// consumed draw flag leaves the reactive predicate false, so a host that
/// paints only on that predicate never paints at all.
#[test]
fn reactive_predicate_alone_can_skip_the_first_frame() {
    let mut h = TestHarness::with_starter_graph();

    // Simulate whatever ran before/during async GPU init consuming the
    // pending draw request (agg-gui clears it on every `App::paint`).
    animation::clear_draw_request();

    let predicate = animation::wants_draw() || h.app_mut().wants_draw();
    assert!(
        !predicate,
        "expected the documented blank-page condition: with the draw \
         request consumed, the reactive predicate is false and an \
         unguarded host would skip the first frame"
    );
}

/// The fix, part one: an explicit `request_draw()` at the end of init is
/// the web equivalent of winit's initial `RedrawRequested`.
#[test]
fn explicit_request_draw_restores_the_reactive_predicate() {
    let mut h = TestHarness::with_starter_graph();
    animation::clear_draw_request();

    animation::request_draw();

    assert!(
        animation::wants_draw() || h.app_mut().wants_draw(),
        "request_draw() must make the host's paint predicate true"
    );
}

/// The fix, part two: the gate paints unconditionally until a frame has
/// actually been presented, so the shell self-heals even if something
/// else consumes the draw flag between init and the next tick.
#[test]
fn first_paint_gate_forces_a_paint_until_a_frame_is_presented() {
    let gate = FirstPaintGate::new();
    animation::clear_draw_request();

    assert!(!gate.has_painted());
    assert!(
        gate.should_paint_tick(false, || false),
        "first tick must paint even with no resize and no draw request"
    );
    // Still not latched — the tick may have bailed out before presenting
    // (no surface texture yet), so the next tick must try again.
    assert!(
        gate.should_paint_tick(false, || false),
        "gate stays open until a frame is actually presented"
    );

    gate.mark_painted();

    assert!(gate.has_painted());
    assert!(
        !gate.should_paint_tick(false, || false),
        "after the first frame the gate defers to the reactive predicate"
    );
    assert!(
        gate.should_paint_tick(true, || false),
        "a resize still forces a paint"
    );
    assert!(
        gate.should_paint_tick(false, || true),
        "a pending draw request still forces a paint"
    );
}

/// agg-gui's `wants_draw()` can promote a due `request_draw_after`
/// deadline into an immediate request, so the gate evaluates the host
/// predicate lazily and skips it entirely while forcing the first frame —
/// keeping the forced tick from perturbing scheduled-draw state.
#[test]
fn forced_first_paint_does_not_evaluate_the_host_predicate() {
    let gate = FirstPaintGate::new();
    let mut evaluated = false;

    assert!(gate.should_paint_tick(false, || {
        evaluated = true;
        false
    }));

    assert!(
        !evaluated,
        "the forced first paint must short-circuit before touching the \
         reactive predicate, which can promote a due deadline"
    );
}
