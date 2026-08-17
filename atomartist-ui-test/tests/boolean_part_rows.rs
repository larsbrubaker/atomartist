//! "Part(s) to Subtract" on the **real canvas** — plan step B-3b of
//! `docs/boolean-node-plan.md`.
//!
//! No NodeDesigner ancestor: the control comes from MatterCAD, where
//! `BooleanObject3D` shows a titled group with one toggle per child
//! (`BooleanObject3D.cs:153-161`). Ours is a per-instance property
//! schema, so the row for an operand exists only while that operand is
//! wired — something a per-*type* schema cannot say.
//!
//! The schema/commit contract is unit-tested next to the node
//! (`atomartist-lib/src/nodes/ops_3d/boolean_rows_tests.rs`). What this
//! file adds is the part only the live widget tree can answer: the rows
//! are actually mounted on the node, and a click landing on one of them
//! changes the selection the node evaluates against.

use agg_gui::widget::find_widget_screen_rect;
use agg_gui::{MouseButton, Point};
use atomartist_lib::graph::graph::Noodle;
use atomartist_lib::graph::node::{NodeId, PortValue};
use atomartist_lib::graph::socket::SocketUid;
use atomartist_lib::nodes::ops_3d::boolean_selection;
use atomartist_lib::registry::NodeRegistry;
use atomartist_lib::Graph;
use atomartist_ui::AppState;
use atomartist_ui_test::{test_storage_registry, TestHarness};
use std::sync::Arc;

/// A canvas holding one Boolean node set to Subtract with two Box
/// operands wired into it.
fn harness_with_subtract() -> (TestHarness, NodeId, Vec<SocketUid>) {
    let mut registry = NodeRegistry::new();
    atomartist_lib::nodes::register_all(&mut registry);

    let mut graph = Graph::new();
    let b = graph
        .add_new_node("Boolean", [140.0, 360.0], &registry)
        .unwrap();
    graph
        .set_property(
            b,
            "operation",
            PortValue::StringVal(Arc::new("Subtract".to_string())),
        )
        .unwrap();
    let mut slots = Vec::new();
    for i in 0..2 {
        let bx = graph
            .add_new_node("Box", [-200.0, 360.0 - 200.0 * i as f64], &registry)
            .unwrap();
        let out = graph.get(bx).unwrap().output_by_name("out").unwrap().uid;
        let slot = graph.get(b).unwrap().inputs.last().unwrap().uid;
        graph
            .connect(Noodle::new(bx, out, b, slot), &registry)
            .unwrap();
        slots.push(slot);
    }

    let state = AppState::with_storage(graph, registry, test_storage_registry());
    let mut h = TestHarness::with_app_state(state);
    h.frame();
    (h, b, slots)
}

/// Every mounted property row of the canvas, as
/// `(property name, row centre in root Y-up coordinates)`.
///
/// The inspector reports node rows in root screen coordinates already
/// (the canvas bakes its pan / zoom into the child bounds), so the
/// centres go straight to `click_local`. Rows outside the canvas
/// viewport are dropped: they are scrolled out of sight, and a click
/// there would land on whatever is actually in front.
fn mounted_rows(h: &TestHarness) -> Vec<(String, Point)> {
    let canvas =
        find_widget_screen_rect(h.app().root(), "node-canvas").expect("the node canvas is mounted");
    h.snapshot()
        .into_iter()
        .filter(|n| n.type_name == "ValueEditorWidget")
        .filter_map(|n| {
            let name = n
                .properties
                .iter()
                .find(|(k, _)| *k == "property")
                .map(|(_, v)| v.clone())?;
            let b = n.screen_bounds;
            let at = Point::new(b.x + b.width * 0.5, b.y + b.height * 0.5);
            let inside = at.x >= canvas.x
                && at.x <= canvas.x + canvas.width
                && at.y >= canvas.y
                && at.y <= canvas.y + canvas.height;
            inside.then_some((name, at))
        })
        .collect()
}

fn selection(h: &TestHarness, id: NodeId) -> String {
    let g = h.state().graph.lock().unwrap();
    match g
        .get(id)
        .unwrap()
        .properties
        .get(boolean_selection::SUBTRACT_PARTS)
    {
        Some(PortValue::StringVal(s)) => s.as_str().to_string(),
        other => panic!("the selection is {:?}", other),
    }
}

/// The rows are on the node: a title plus one checkbox per operand,
/// mounted as real widgets the user can hit.
#[test]
fn the_canvas_mounts_a_checkbox_row_per_operand() {
    let (h, _id, slots) = harness_with_subtract();
    let rows = mounted_rows(&h);
    let names: Vec<&str> = rows.iter().map(|(n, _)| n.as_str()).collect();
    assert!(
        names.contains(&boolean_selection::HEADER_ROW),
        "no group title row among {:?}",
        names
    );
    for slot in &slots {
        let row = format!("{}{}", boolean_selection::ROW_PREFIX, slot.0);
        assert!(
            names.contains(&row.as_str()),
            "operand {:?} has no row among {:?}",
            slot,
            names
        );
    }
}

/// Clicking one changes the selection the node evaluates against —
/// through the real canvas dispatcher, the real toggle branch, and the
/// real property-commit path.
#[test]
fn clicking_a_checkbox_row_changes_the_selection() {
    let (mut h, id, slots) = harness_with_subtract();
    assert_eq!(
        selection(&h, id),
        boolean_selection::AUTO,
        "nobody has chosen yet"
    );

    let row = format!("{}{}", boolean_selection::ROW_PREFIX, slots[0].0);
    let at = mounted_rows(&h)
        .into_iter()
        .find(|(n, _)| *n == row)
        .map(|(_, at)| at)
        .expect("the first operand's row is mounted");
    h.click_local(at, MouseButton::Left);

    assert_eq!(
        selection(&h, id),
        boolean_selection::encode(&slots),
        "the click must materialize the auto default and add the part it flipped"
    );

    // …and the row it drew comes back checked, so a second click on
    // the same spot unchecks it. A row that redrew unchecked would send
    // `true` again and the selection would not move.
    h.click_local(at, MouseButton::Left);
    assert_eq!(
        selection(&h, id),
        boolean_selection::encode(&slots[1..]),
        "the second click on the same row must uncheck the part it checked"
    );
}

/// Switching to an operation that does not cut takes the whole group
/// away — MatterCAD's `UpdateControls` gate, live on the canvas.
#[test]
fn the_rows_disappear_for_an_operation_that_does_not_cut() {
    let (mut h, id, _slots) = harness_with_subtract();
    {
        let mut g = h.state().graph.lock().unwrap();
        g.set_property(
            id,
            "operation",
            PortValue::StringVal(Arc::new("Combine".to_string())),
        )
        .unwrap();
    }
    h.frame();

    let names: Vec<String> = mounted_rows(&h).into_iter().map(|(n, _)| n).collect();
    assert!(
        !names.iter().any(|n| boolean_selection::is_row(n)),
        "Combine has nothing to subtract, yet the rows are still mounted: {:?}",
        names
    );
}
