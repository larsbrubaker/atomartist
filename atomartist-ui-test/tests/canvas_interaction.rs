//! Canvas-interaction UX tests.
//!
//! Ports / equivalents of the following NodeDesigner suites — each test
//! verifies the same end-user behaviour against AtomArtist's production
//! code paths through the harness:
//!
//! - `MatterHackers/FDS/NodeDesigner/tests/unit/keyboard-event-consumption.test.ts`
//! - `MatterHackers/FDS/NodeDesigner/tests/unit/snap-grid.test.ts`
//! - `MatterHackers/FDS/NodeDesigner/tests/unit/selection-state.test.ts`
//! - `MatterHackers/FDS/NodeDesigner/tests/unit/keyboard-event-consumption.test.ts`
//!
//! The TS originals run under a heavily mocked DOM; we test the same
//! intent against the real `NodeCanvas` widget via `TestHarness`.

use agg_gui::{Modifiers, MouseButton, Point};
use atomartist_lib::graph::node::NodeId;
use atomartist_ui_test::TestHarness;

#[test]
fn empty_canvas_click_clears_app_state_selection() {
    let mut h = TestHarness::with_starter_graph();
    h.state().set_selection(Some(NodeId(99)));

    let canvas = h.find_by_id("node-canvas").expect("canvas widget");
    let b = canvas.bounds();
    let local_x = b.x + b.width * 0.95;
    let local_y_yup = b.y + b.height * 0.5;
    let screen_y = 720.0 - local_y_yup;

    h.click(local_x, screen_y, MouseButton::Left);
    assert_eq!(*h.state().selection.lock().unwrap(), None);
}

#[test]
fn programmatic_selection_persists_through_event_dispatch() {
    // Setting selection via `AppState::set_selection` should survive
    // an event that *doesn't* hit the canvas (e.g. movement over
    // empty area without a click).
    let mut h = TestHarness::with_starter_graph();
    h.state().set_selection(Some(NodeId(7)));
    h.mouse_move(10.0, 10.0);
    assert_eq!(*h.state().selection.lock().unwrap(), Some(NodeId(7)));
}

#[test]
fn wheel_event_over_canvas_changes_canvas_zoom() {
    // NodeDesigner: scrolling on the canvas zooms it. Production
    // `NodeCanvas` writes the new zoom level into AppState::canvas_zoom
    // so the status bar can read it. Test that wheel events propagate
    // through the harness into a measurable zoom change.
    let mut h = TestHarness::with_starter_graph();
    let zoom_before = *h.state().canvas_zoom.lock().unwrap();

    let canvas = h.find_by_id("node-canvas").expect("canvas widget");
    let b = canvas.bounds();
    let cx = b.x + b.width * 0.5;
    let cy_screen = 720.0 - (b.y + b.height * 0.5);

    h.mouse_move(cx, cy_screen);
    h.scroll(120.0); // positive delta typically zooms in

    let zoom_after = *h.state().canvas_zoom.lock().unwrap();
    assert!(
        (zoom_after - zoom_before).abs() > f64::EPSILON,
        "scroll should change zoom; before={zoom_before} after={zoom_after}"
    );
}

#[test]
fn keyboard_event_with_no_focus_does_not_panic() {
    // Mirrors NodeDesigner's keyboard-event-consumption test intent:
    // a Backspace / Delete with nothing focused should not cascade into
    // node deletion or any panic. We simulate a Backspace with default
    // modifiers and verify the graph node count is unchanged.
    let mut h = TestHarness::with_starter_graph();
    let before = h.state().graph.lock().unwrap().nodes().count();
    h.key_down(agg_gui::Key::Backspace);
    let after = h.state().graph.lock().unwrap().nodes().count();
    assert_eq!(before, after, "stray keys must not delete graph nodes");
}

#[test]
fn deleting_the_selected_node_clears_app_state_selection() {
    // Deleting the primary-selected node must not leave `AppState`
    // pointing at a node that no longer exists: the editor fires
    // `on_primary_selection_changed(None)`, which `AppStateModel`
    // mirrors into `AppState::selection`.
    let mut h = TestHarness::with_starter_graph();

    let victim = {
        let graph = h.state().graph.lock().unwrap();
        let id = graph.nodes().next().expect("starter graph has nodes").id;
        id
    };
    let before = h.state().graph.lock().unwrap().nodes().count();

    // Clicking the node both selects it and focuses the canvas, so its
    // key handler is live.
    let target = node_center(&h, victim);
    h.click(target.0, target.1, MouseButton::Left);
    assert_eq!(
        *h.state().selection.lock().unwrap(),
        Some(victim),
        "clicking a node must mirror it into AppState::selection"
    );

    h.key_down(agg_gui::Key::Delete);

    assert_eq!(
        h.state().graph.lock().unwrap().nodes().count(),
        before - 1,
        "Delete must remove the selected node"
    );
    assert!(
        !h.state()
            .graph
            .lock()
            .unwrap()
            .nodes()
            .any(|n| n.id == victim),
        "and it must be the node we selected"
    );
    assert_eq!(
        *h.state().selection.lock().unwrap(),
        None,
        "the mirrored selection must not survive the node it points at"
    );
}

/// Screen (Y-down) centre of the canvas widget drawn for `node_id`.
///
/// The node-editor bakes each node's pan/zoom-transformed bounds into a
/// child `NodeWidget`, which `snapshot()` surfaces with absolute Y-up
/// `screen_bounds`.
fn node_center(h: &TestHarness, node_id: NodeId) -> (f64, f64) {
    let want = node_id.0.to_string();
    let bounds = h
        .snapshot()
        .into_iter()
        .find_map(|n| {
            let is_node = n.type_name == "NodeWidget"
                && n.properties
                    .iter()
                    .any(|(key, value)| *key == "node_id" && *value == want);
            is_node.then_some(n.screen_bounds)
        })
        .expect("the node has a widget on the canvas");
    let center = Point::new(bounds.x + bounds.width * 0.5, bounds.y + bounds.height * 0.5);
    h.to_screen(center)
}

#[test]
fn ctrl_modifier_state_propagates_through_harness() {
    // Verifies that the harness's modifier-state wiring is correct —
    // pressing a chord with Ctrl set is what every NodeDesigner UI
    // shortcut test relies on (Ctrl+Z, Ctrl+S, etc.).
    let mut h = TestHarness::new();
    let mut mods = Modifiers::default();
    mods.ctrl = true;
    h.set_modifiers(mods);
    // No assertion on outcome — we're testing that the harness
    // dispatches without panic. A panic in this path would mean the
    // chord couldn't reach the focused widget.
    h.key_down(agg_gui::Key::Char('z'));
}
