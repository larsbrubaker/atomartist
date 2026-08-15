//! Edit-menu end-to-end tests — "Delete Selected" and "Select All".
//!
//! Equivalent of NodeDesigner's
//! `MatterHackers/FDS/NodeDesigner/tests/unit/selection-state.test.ts`
//! menu half: the Edit menu must reach the canvas's *widget-side*
//! multi-selection, which `AppState` does not mirror (it keeps only the
//! primary selection).
//!
//! The menu callback has no access to the widget tree, so the actions
//! travel as queued commands on a `NodeEditorHandle` that the editor
//! drains at the start of each `layout()`. Every test here therefore
//! drives `TestHarness::menu_action` (which routes through the
//! production `menu_actions::handle_action` and then runs a frame) and
//! asserts against observable state: the graph itself, and the
//! per-`NodeWidget` `selected` flag the inspector snapshot exposes.

use agg_gui::{MouseButton, Rect};
use atomartist_lib::graph::node::NodeId;
use atomartist_ui_test::TestHarness;

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

/// `(node_id, selected)` for every node widget the canvas is painting.
fn selection_flags(h: &TestHarness) -> Vec<(String, bool)> {
    h.snapshot()
        .into_iter()
        .filter(|n| n.type_name == "NodeWidget")
        .map(|n| {
            let id = n
                .properties
                .iter()
                .find(|(k, _)| *k == "node_id")
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            let selected = n
                .properties
                .iter()
                .any(|(k, v)| *k == "selected" && v == "true");
            (id, selected)
        })
        .collect()
}

fn node_ids(h: &TestHarness) -> Vec<NodeId> {
    h.state()
        .graph
        .lock()
        .unwrap()
        .nodes()
        .map(|n| n.id)
        .collect()
}

#[test]
fn edit_select_all_marks_every_node_selected() {
    let mut h = TestHarness::with_starter_graph();
    h.frame();
    let before = selection_flags(&h);
    assert!(
        before.len() >= 2,
        "starter graph should paint at least two node widgets, got {}",
        before.len()
    );
    assert!(
        before.iter().all(|(_, sel)| !*sel),
        "nothing should start selected: {before:?}"
    );

    h.menu_action("edit.select_all");

    let after = selection_flags(&h);
    assert_eq!(after.len(), before.len(), "no node should disappear");
    assert!(
        after.iter().all(|(_, sel)| *sel),
        "Select All must select every node: {after:?}"
    );
}

#[test]
fn edit_delete_removes_every_selected_node() {
    let mut h = TestHarness::with_starter_graph();
    h.frame();
    let before = node_ids(&h).len();
    assert!(before >= 2, "starter graph should have nodes");

    h.menu_action("edit.select_all");
    h.menu_action("edit.delete");

    assert_eq!(
        node_ids(&h).len(),
        0,
        "Delete Selected after Select All must empty the graph"
    );

    // Deletion goes through the model, which wraps a multi-node delete
    // into a single `BatchCmd` named "Delete N Nodes" — so one Undo
    // brings the whole selection back. This matches NodeDesigner, whose
    // selection.js groups a multi-delete into one "Delete N nodes" batch.
    h.menu_action("edit.undo");
    assert_eq!(
        node_ids(&h).len(),
        before,
        "undo must restore every deleted node"
    );
}

#[test]
fn edit_delete_removes_only_the_clicked_node() {
    let mut h = TestHarness::with_starter_graph();
    h.frame();
    let ids = node_ids(&h);
    assert!(ids.len() >= 2, "need at least two nodes to tell them apart");
    let target = ids[0];

    let b = node_bounds(&h, target).expect("target node must be painted");
    let (sx, sy) = h.to_screen(agg_gui::Point::new(
        b.x + b.width * 0.5,
        b.y + b.height * 0.5,
    ));
    h.click(sx, sy, MouseButton::Left);

    h.menu_action("edit.delete");

    let after = node_ids(&h);
    assert_eq!(after.len(), ids.len() - 1, "exactly one node should go");
    assert!(
        !after.contains(&target),
        "the clicked node {target:?} should be the one removed"
    );
}

#[test]
fn edit_delete_with_no_selection_is_harmless() {
    let mut h = TestHarness::with_starter_graph();
    h.frame();
    let before = node_ids(&h).len();
    h.menu_action("edit.delete");
    assert_eq!(node_ids(&h).len(), before, "empty selection deletes nothing");
}
