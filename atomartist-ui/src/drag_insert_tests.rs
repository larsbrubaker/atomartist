//! Unit tests for the drag-insert controller
//! ([`crate::drag_insert`], design step 6e).
//!
//! These drive the controller directly (no widget tree) so the state
//! machine — threshold, insert on canvas-enter, base-position move math,
//! remove on leave, single-undo commit — is pinned independently of how
//! the bar and the browser feed it. The gesture as the *user* performs
//! it (real events through the real widget tree) is covered by
//! `atomartist-ui-test/tests/drag_insert.rs`.
//!
//! Geometry used throughout: a 44 px bar with the canvas rectangle
//! published immediately to its right inside a 800 × 300 pane, so
//! bar-local x < 44 is "over the bar" and anything beyond is canvas.
//! Y-up, as everywhere else. The controller only ever sees a rectangle,
//! so this fixture and production's (canvas *below* the bar, since step
//! 6f-1 moved it to the 3-D pane) exercise the same code — the
//! below-the-bar shape gets its own test at the end.

use super::*;

use crate::top_level::fresh_state_with_builtins;

const BAR_W: f64 = 44.0;
const PANE_W: f64 = 800.0;
const PANE_H: f64 = 300.0;

/// The canvas as this fixture publishes it: the rest of the pane to the
/// bar's right.
fn canvas_rect() -> Rect {
    Rect::new(BAR_W, 0.0, PANE_W - BAR_W, PANE_H)
}

fn controller() -> (AppState, DragInsertHandle) {
    let (state, handle, _overlay) = controller_with_overlay();
    (state, handle)
}

/// Same, but keeping the overlay handle so a test can assert on what the
/// ghost queued into (and withdrew from) the app's floating slot.
fn controller_with_overlay() -> (AppState, DragInsertHandle, FloatingOverlayHandle) {
    let state = fresh_state_with_builtins();
    let overlay = FloatingOverlayHandle::new();
    let handle = DragInsertHandle::new(state.clone(), overlay.clone());
    handle.set_canvas_rect(canvas_rect());
    (state, handle, overlay)
}

fn box_payload() -> DragPayload {
    DragPayload::NodeType {
        type_id: "Box".to_string(),
        label: "Box".to_string(),
        glyph: 'B',
    }
}

fn node_count(state: &AppState) -> usize {
    state.graph.lock().unwrap().node_count()
}

/// A press released before the threshold is a click: no ghost, no
/// insert, and the caller is told to run its own click behaviour.
#[test]
fn sub_threshold_release_is_a_click() {
    let (state, handle) = controller();
    handle.press(box_payload(), Point::new(20.0, 200.0));
    // Two nudges, both inside the 4 px threshold.
    handle.pointer_move(Point::new(21.0, 200.0));
    handle.pointer_move(Point::new(22.0, 201.0));
    assert!(!handle.is_dragging(), "still a click, not a drag");
    assert!(!handle.ghost_active());
    assert_eq!(
        handle.pointer_up(Point::new(22.0, 201.0)),
        GestureEnd::Click
    );
    assert_eq!(node_count(&state), 0, "a click must insert nothing");
}

/// Past the threshold, outside the canvas: the ghost is up and the
/// gesture reports itself as dragging.
#[test]
fn crossing_the_threshold_raises_the_ghost() {
    let (state, handle) = controller();
    handle.press(box_payload(), Point::new(20.0, 200.0));
    handle.pointer_move(Point::new(20.0, 180.0));
    assert!(handle.is_dragging());
    assert!(handle.ghost_active(), "ghost follows the cursor off-canvas");
    assert_eq!(node_count(&state), 0, "nothing inserted while outside");
}

/// Crossing into the canvas inserts the real node at the cursor and
/// drops the ghost (design §2: "real insertion on canvas-enter").
#[test]
fn entering_the_canvas_inserts_the_node() {
    let (state, handle) = controller();
    handle.press(box_payload(), Point::new(20.0, 200.0));
    handle.pointer_move(Point::new(20.0, 180.0));
    handle.pointer_move(Point::new(300.0, 180.0));

    assert_eq!(node_count(&state), 1);
    assert!(!handle.ghost_active(), "ghost gives way to the real node");
    let id = handle.carried_node().expect("node is being carried");
    let g = state.graph.lock().unwrap();
    let node = g.get(id).unwrap();
    assert_eq!(node.type_id.as_ref(), "Box");
    // Canvas x = bar-local x - bar width (pan 0, zoom 1).
    assert_eq!(node.position, [300.0 - BAR_W, 180.0]);
}

/// Dragging back out removes the node again and the ghost returns —
/// the ancestor's "leave = remove + re-ghost".
#[test]
fn leaving_the_canvas_removes_the_node_and_reghosts() {
    let (state, handle) = controller();
    handle.press(box_payload(), Point::new(20.0, 200.0));
    handle.pointer_move(Point::new(20.0, 180.0));
    handle.pointer_move(Point::new(300.0, 180.0));
    assert_eq!(node_count(&state), 1);

    handle.pointer_move(Point::new(20.0, 180.0));
    assert_eq!(node_count(&state), 0, "the carried node is taken away");
    assert!(handle.ghost_active());
    assert!(handle.carried_node().is_none());
}

/// Release inside the canvas commits — and the whole gesture is exactly
/// one undo step, no matter how many intermediate inserts / moves it
/// took.
#[test]
fn commit_is_a_single_undo_step() {
    let (state, handle) = controller();
    handle.press(box_payload(), Point::new(20.0, 200.0));
    handle.pointer_move(Point::new(20.0, 180.0));
    // In, out, and back in again: three inserts / removals.
    handle.pointer_move(Point::new(300.0, 180.0));
    handle.pointer_move(Point::new(20.0, 180.0));
    handle.pointer_move(Point::new(320.0, 160.0));
    assert_eq!(
        handle.pointer_up(Point::new(320.0, 160.0)),
        GestureEnd::Dropped
    );

    assert_eq!(node_count(&state), 1);
    let undo = state.active_undo();
    assert!(undo.lock().unwrap().can_undo());
    undo.lock().unwrap().undo();
    assert_eq!(node_count(&state), 0, "one undo removes the whole gesture");
    assert!(
        !undo.lock().unwrap().can_undo(),
        "the gesture pushed exactly one entry"
    );
}

/// Release outside the canvas cancels: nothing inserted, nothing to
/// undo.
#[test]
fn release_outside_the_canvas_inserts_nothing() {
    let (state, handle) = controller();
    handle.press(box_payload(), Point::new(20.0, 200.0));
    handle.pointer_move(Point::new(20.0, 180.0));
    handle.pointer_move(Point::new(300.0, 180.0));
    assert_eq!(node_count(&state), 1);

    handle.pointer_move(Point::new(20.0, 120.0));
    assert_eq!(
        handle.pointer_up(Point::new(20.0, 120.0)),
        GestureEnd::Cancelled
    );
    assert_eq!(node_count(&state), 0);
    assert!(!state.active_undo().lock().unwrap().can_undo());
    assert!(!handle.is_pressed());
}

/// Escape mid-drag removes the carried node and drops the ghost.
#[test]
fn escape_cancels_the_gesture() {
    let (state, handle) = controller();
    handle.press(box_payload(), Point::new(20.0, 200.0));
    handle.pointer_move(Point::new(20.0, 180.0));
    handle.pointer_move(Point::new(300.0, 180.0));
    assert_eq!(node_count(&state), 1);

    assert!(handle.cancel());
    assert_eq!(node_count(&state), 0);
    assert!(!handle.is_pressed());
    assert!(!handle.is_dragging());
    assert!(!handle.cancel(), "nothing left to cancel");
    assert!(!state.active_undo().lock().unwrap().can_undo());
}

/// The move math works from a base-position snapshot, so a long wiggle
/// leaves the node exactly under the cursor — no accumulated drift.
#[test]
fn wiggling_never_accumulates_drift() {
    let (state, handle) = controller();
    handle.press(box_payload(), Point::new(20.0, 200.0));
    handle.pointer_move(Point::new(20.0, 180.0));
    handle.pointer_move(Point::new(300.0, 180.0));

    for i in 0..200 {
        let x = 300.0 + ((i % 17) as f64) * 3.0;
        let y = 180.0 - ((i % 11) as f64) * 2.0;
        handle.pointer_move(Point::new(x, y));
    }
    let last = Point::new(412.5, 143.25);
    handle.pointer_move(last);

    let id = handle.carried_node().unwrap();
    let g = state.graph.lock().unwrap();
    let pos = g.get(id).unwrap().position;
    assert!(
        (pos[0] - (last.x - BAR_W)).abs() < 1e-9 && (pos[1] - last.y).abs() < 1e-9,
        "node must sit exactly at the cursor, got {pos:?}"
    );
}

/// A panned / zoomed canvas maps the drop point through the live view,
/// the way the OS file-drop path does with `local_to_canvas`.
#[test]
fn drop_position_respects_pan_and_zoom() {
    let (state, handle) = controller();
    *state.canvas_zoom.lock().unwrap() = 2.0;
    *state.canvas_pan.lock().unwrap() = [40.0, -20.0];

    handle.press(box_payload(), Point::new(20.0, 200.0));
    handle.pointer_move(Point::new(20.0, 180.0));
    handle.pointer_move(Point::new(300.0, 180.0));
    handle.pointer_up(Point::new(300.0, 180.0));

    let g = state.graph.lock().unwrap();
    let node = g.nodes().next().expect("one node inserted");
    // local = (300 - 44, 180); canvas = (local - pan) / zoom.
    assert_eq!(node.position, [(256.0 - 40.0) / 2.0, (180.0 + 20.0) / 2.0]);
}

/// A file payload is never carried live — the ghost stays all the way
/// to the release, where the import path takes over.
#[test]
fn file_payload_keeps_its_ghost_over_the_canvas() {
    let (state, handle) = controller();
    let uri = StorageUri::new("mem", "/models/part.stl");
    handle.press(
        DragPayload::File {
            uri,
            label: "part.stl".to_string(),
            glyph: 'F',
        },
        Point::new(20.0, 200.0),
    );
    handle.pointer_move(Point::new(20.0, 180.0));
    handle.pointer_move(Point::new(300.0, 180.0));

    assert!(handle.ghost_active(), "no live carry for async payloads");
    assert!(handle.carried_node().is_none());
    assert_eq!(node_count(&state), 0);
}

/// A second press while a gesture is live (agg-gui has one capture slot,
/// so a right-button press mid-drag steals it and the left release never
/// arrives) must not orphan the carried node: the old gesture is ended
/// first, and the new one starts clean.
#[test]
fn a_second_press_ends_the_gesture_in_flight() {
    let (state, handle, overlay) = controller_with_overlay();
    handle.press(box_payload(), Point::new(20.0, 200.0));
    handle.pointer_move(Point::new(20.0, 180.0));
    handle.pointer_move(Point::new(300.0, 180.0));
    assert_eq!(node_count(&state), 1, "a node is being carried");

    // The capture-stealing press.
    handle.press(box_payload(), Point::new(20.0, 100.0));

    assert_eq!(node_count(&state), 0, "the orphaned node is taken back");
    assert!(
        !state.active_undo().lock().unwrap().can_undo(),
        "and nothing reached the undo stack"
    );
    assert!(
        !handle.is_dragging(),
        "the new gesture starts as a candidate"
    );
    assert!(!handle.ghost_active());
    assert!(!overlay.is_pending(), "the old ghost was withdrawn");

    // The fresh gesture still works end to end.
    handle.pointer_move(Point::new(20.0, 80.0));
    handle.pointer_move(Point::new(310.0, 90.0));
    assert_eq!(
        handle.pointer_up(Point::new(310.0, 90.0)),
        GestureEnd::Dropped
    );
    assert_eq!(node_count(&state), 1);
    assert!(state.active_undo().lock().unwrap().can_undo());
}

/// Hiding the ghost must also withdraw a spawn the overlay host has not
/// claimed yet (it is busy showing the colour picker) — otherwise the
/// ghost would appear, ownerless, whenever that dialog closes.
#[test]
fn hiding_the_ghost_withdraws_an_unclaimed_spawn() {
    let (_state, handle, overlay) = controller_with_overlay();
    handle.press(box_payload(), Point::new(20.0, 200.0));
    handle.pointer_move(Point::new(20.0, 180.0));
    assert!(overlay.is_pending(), "the ghost is queued for the host");

    // Crossing into the canvas retires the ghost in favour of the node.
    handle.pointer_move(Point::new(300.0, 180.0));
    assert!(
        !overlay.is_pending(),
        "the queued ghost is withdrawn, not just flagged"
    );
}

/// A sub-threshold press on the bar released over the *canvas* is not a
/// click on the row: it must not activate it.
#[test]
fn press_then_teleport_into_the_canvas_is_not_a_click() {
    let (state, handle) = controller();
    handle.press(box_payload(), Point::new(20.0, 200.0));
    // No move at all — straight to a release far away, as a synthetic
    // event stream (or a pointer warp) can produce.
    assert_eq!(
        handle.pointer_up(Point::new(300.0, 200.0)),
        GestureEnd::Cancelled
    );
    assert_eq!(node_count(&state), 0);
    assert!(!state.active_undo().lock().unwrap().can_undo());
}

/// A payload nothing can import must not report success: the gesture
/// ends cancelled and the user is told why.
#[test]
fn an_unimportable_file_payload_is_reported_not_swallowed() {
    let (state, handle) = controller();
    handle.press(
        DragPayload::File {
            uri: StorageUri::new("mem", "/notes/readme.txt"),
            label: "readme.txt".to_string(),
            glyph: 'F',
        },
        Point::new(20.0, 200.0),
    );
    handle.pointer_move(Point::new(20.0, 180.0));
    handle.pointer_move(Point::new(300.0, 180.0));
    assert_eq!(
        handle.pointer_up(Point::new(300.0, 180.0)),
        GestureEnd::Cancelled,
        "an import that could not run is not a drop"
    );
    assert_eq!(node_count(&state), 0);
    let notices = state.drain_notices();
    assert!(
        notices.iter().any(|n| n.text.contains("readme.txt")),
        "the refusal must reach the user: {notices:?}"
    );
}

/// The 6f-1 shape: the bar is docked in the 3-D viewport pane and the
/// canvas sits in the pane *below* it, i.e. at negative bar-local `y`.
/// The controller must treat that rectangle exactly like any other —
/// dropping there inserts at the cursor, and a point between the two
/// panes (the splitter's divider) is not the canvas.
#[test]
fn a_canvas_below_the_bar_is_still_the_drop_target() {
    let (state, handle) = controller();
    // Viewport pane 0..300 (bar-local y), 6 px divider, canvas pane
    // spanning y ∈ [-206, -6].
    let canvas = Rect::new(0.0, -206.0, PANE_W, 200.0);
    handle.set_canvas_rect(canvas);

    handle.press(box_payload(), Point::new(20.0, 200.0));
    handle.pointer_move(Point::new(20.0, 180.0));
    assert!(handle.ghost_active(), "still over the viewport pane");
    // Over the divider between the panes: not a drop target.
    handle.pointer_move(Point::new(300.0, -3.0));
    assert!(handle.carried_node().is_none());

    handle.pointer_move(Point::new(300.0, -100.0));
    let id = handle.carried_node().expect("the canvas below accepts it");
    {
        let g = state.graph.lock().unwrap();
        // Canvas-local = bar-local minus the canvas origin (pan 0, zoom 1).
        assert_eq!(g.get(id).unwrap().position, [300.0, -100.0 + 206.0]);
    }
    assert_eq!(
        handle.pointer_up(Point::new(300.0, -100.0)),
        GestureEnd::Dropped
    );
    assert_eq!(node_count(&state), 1);
}

/// `.mcx` is importable, so it is draggable — the two lists are one.
#[test]
fn every_importable_extension_is_draggable() {
    use crate::app_state_files_import::is_importable_extension;
    for ext in ["stl", "obj", "3mf", "mcx", "atmr"] {
        assert!(is_importable_extension(ext), ".{ext} must be importable");
    }
    assert!(!is_importable_extension("txt"));
}
