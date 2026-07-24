//! Integration coverage for the "only the Output node renders" viewport
//! display policy (product spec). Two guarantees:
//!
//! 1. A primitive sitting on the canvas that is NOT wired into the Output
//!    node must never appear in the 3-D viewport — not even while it is the
//!    selected node (the old preview-on-selection behaviour violated this).
//! 2. Multiple primitives wired into the Output node all render: Output
//!    concatenates every connected `Geometry3d` body into its merged
//!    `__display__` group, and `pick_display_mesh` shows exactly that.
//!
//! Regression guard for the menu-add bug cluster (unconnected Cylinder
//! rendering, second body not appearing).

use agg_gui_node_editor as ne;
use agg_gui_node_editor::NodeGraphModel;
use atomartist_ui::{fresh_state_with_builtins, fresh_state_with_starter_graph, AppStateModel};

/// The root graph's Output node id, as the editor's opaque `NodeId`.
fn output_node(state: &atomartist_ui::AppState) -> ne::NodeId {
    let g = state.graph.lock().unwrap();
    let id = g
        .nodes()
        .find(|n| n.type_id.as_ref() == "Output")
        .expect("starter graph has an Output node")
        .id;
    ne::NodeId(id.0)
}

/// Guarantee #1 in its most literal form: an Output node with ZERO
/// connected inputs contributes no geometry, so the viewport displays
/// nothing at all (`pick_display_mesh` returns `None`).
#[test]
fn output_with_no_connected_inputs_displays_nothing() {
    let state = fresh_state_with_builtins();
    let mut model = AppStateModel::new(state.clone());
    model.add_node("Output", [80.0, 240.0]).expect("output added");

    state.evaluate_now();

    let displayed = state.last_mesh_output.lock().unwrap().clone();
    let has_geometry = displayed
        .map(|m| {
            !m.is_empty()
                && m.iter()
                    .any(|b| atomartist_lib::geometry::num_tris(&b.mesh) > 0)
        })
        .unwrap_or(false);
    assert!(
        !has_geometry,
        "an Output node with no connected inputs must not display any mesh",
    );
}

#[test]
fn unconnected_primitive_never_renders_but_connecting_to_output_does() {
    let state = fresh_state_with_starter_graph();

    // Baseline: the starter pipeline (…→ Output) renders exactly one body.
    state.evaluate_now();
    let baseline_bodies = state
        .last_mesh_output
        .lock()
        .unwrap()
        .clone()
        .expect("starter graph renders a mesh")
        .len();
    assert!(baseline_bodies >= 1, "starter graph should render at least one body");

    let mut model = AppStateModel::new(state.clone());

    // Add a Box in open canvas space and select it. With preview-on-
    // selection removed, selecting an unconnected primitive must not pin
    // it as the viewport display, so the rendered body count is unchanged.
    let box_id = model.add_node("Box", [80.0, 480.0]).expect("box added");
    model.on_primary_selection_changed(Some(box_id));
    state.evaluate_now();
    let after_select = state
        .last_mesh_output
        .lock()
        .unwrap()
        .clone()
        .expect("output still renders");
    assert_eq!(
        after_select.len(),
        baseline_bodies,
        "selecting an unconnected primitive must not render it",
    );

    // Wire the Box into the Output node's trailing empty placeholder input
    // (name ""). This is the exact connection the user could not make when
    // the node was hidden behind another; the connect logic itself is fine.
    let out = output_node(&state);
    let result = model.try_add_noodle(box_id, "out", out, "");
    assert!(
        matches!(result, ne::NoodleResult::Connected | ne::NoodleResult::Replaced),
        "connecting the Box output to Output should succeed, got {:?}",
        result,
    );

    // Now BOTH bodies render — the starter plate plus the Box.
    state.evaluate_now();
    let after_connect = state
        .last_mesh_output
        .lock()
        .unwrap()
        .clone()
        .expect("output renders merged geometry");
    assert_eq!(
        after_connect.len(),
        baseline_bodies + 1,
        "connecting a second primitive to Output must render both bodies",
    );
}
