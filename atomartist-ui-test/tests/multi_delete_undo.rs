//! End-to-end coverage for "a multi-node delete is one undo step".
//!
//! NodeDesigner source: `static/js/node-editor/graph/state/selection.js`
//! (`deleteSelectedNodes` — opens a batch named "Delete N nodes" so a
//! single Ctrl+Z brings the whole selection back).
//!
//! Drives the production widget tree via `TestHarness`: shift-click each
//! node's title bar to build a multi-selection, press Delete (agg-gui's
//! `NodeEditor` funnels the key, the right-click menu, and the Edit-menu
//! command through one `delete_selection` → `NodeGraphModel::remove_nodes`
//! call), then undo once through the real `edit.undo` menu action.
//!
//! Coordinates: `snapshot()` reports absolute **Y-up** screen bounds;
//! the harness's click helpers take Y-down screen coordinates, hence the
//! `DEFAULT_HEIGHT - y` flip.

use agg_gui::{Key, Modifiers, MouseButton, Rect};
use atomartist_lib::graph::node::NodeId;
use atomartist_ui_test::harness::DEFAULT_HEIGHT;
use atomartist_ui_test::TestHarness;

/// Height of a node's title bar in canvas-space (mirrors the node-editor
/// crate's private `draw::TITLE_HEIGHT`).
const TITLE_HEIGHT: f64 = 26.0;

/// Absolute (Y-up) screen bounds of the `NodeWidget` for `node_id`.
fn node_bounds(h: &TestHarness, node_id: NodeId) -> Option<Rect> {
    let want = node_id.0.to_string();
    h.snapshot().into_iter().find_map(|n| {
        if n.type_name != "NodeWidget" {
            return None;
        }
        let matches = n
            .properties
            .iter()
            .any(|(k, v)| *k == "node_id" && *v == want);
        matches.then_some(n.screen_bounds)
    })
}

/// Screen point (Y-down) on a node's title bar — centred so we clear the
/// collapse chevron on the far left and miss the interior value rows.
fn title_bar_point(sb: Rect) -> (f64, f64) {
    let wx = sb.x + sb.width * 0.5;
    let wy = sb.y + sb.height - TITLE_HEIGHT * 0.5;
    (wx, DEFAULT_HEIGHT - wy)
}

fn node_ids(h: &TestHarness) -> Vec<NodeId> {
    let g = h.state().graph.lock().unwrap();
    let mut ids: Vec<NodeId> = g.nodes().map(|n| n.id).collect();
    ids.sort_by_key(|id| id.0);
    ids
}

#[test]
fn deleting_a_multi_selection_undoes_in_one_step() {
    let mut h = TestHarness::with_starter_graph();
    h.frame();

    let before = node_ids(&h);
    assert!(
        before.len() >= 3,
        "starter graph should have several nodes; got {}",
        before.len()
    );
    let noodles_before = h.state().graph.lock().unwrap().noodles().len();

    // Shift-click every node's title bar to build the multi-selection.
    let mut shift = Modifiers::default();
    shift.shift = true;
    h.set_modifiers(shift);
    for id in &before {
        let sb = node_bounds(&h, *id).unwrap_or_else(|| panic!("no NodeWidget for {id:?}"));
        let (x, y) = title_bar_point(sb);
        h.click(x, y, MouseButton::Left);
    }
    h.clear_modifiers();

    // Delete key → NodeEditor::delete_selection → remove_nodes.
    h.key_down(Key::Delete);
    assert_eq!(
        h.state().graph.lock().unwrap().nodes().count(),
        0,
        "the whole selection must be deleted",
    );

    // Exactly one undo restores the entire graph.
    h.menu_action("edit.undo");
    assert_eq!(
        node_ids(&h),
        before,
        "a single undo must restore every deleted node",
    );
    assert_eq!(
        h.state().graph.lock().unwrap().noodles().len(),
        noodles_before,
        "a single undo must restore the noodles too",
    );
}
