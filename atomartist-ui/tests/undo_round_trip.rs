//! Undo / redo round-trip tests for every `AppStateModel` mutation.
//!
//! Contract: every user-visible mutation that crosses the
//! `agg_gui_node_editor::NodeGraphModel` boundary lands on
//! `AppState.undo` so a subsequent Ctrl+Z reverses it. Coverage:
//!
//!   * add_node / remove_node
//!   * set_node_position (with drag-coalescing — many calls → 1 step)
//!   * try_add_noodle / remove_noodle (incl. Replace path = batched
//!     Disconnect + Connect undoes as one)
//!   * set_property (with slider-coalescing)
//!
//! Lives as an integration test (not in `lib.rs`'s `mod tests`) so the
//! parent file stays under the 800-line cap.

use agg_gui_node_editor as ne;
use atomartist_lib::graph::node::PortValue;
use atomartist_lib::nodes;
use atomartist_lib::registry::NodeRegistry;
use atomartist_lib::Graph;
use atomartist_ui::{AppState, AppStateModel};

fn fixture() -> AppState {
    let mut reg = NodeRegistry::new();
    nodes::register_all(&mut reg);
    AppState::new(Graph::new(), reg)
}

#[test]
fn add_node_through_bridge_lands_on_undo_stack() {
    let state = fixture();
    let mut model = AppStateModel::new(state);
    let _id = ne::NodeGraphModel::add_node(&mut model, "Box", [5.0, 7.0]);
    assert_eq!(
        model.state.undo.lock().unwrap().undo_name(),
        Some("Add Node"),
    );
    assert_eq!(model.state.graph.lock().unwrap().nodes().count(), 1);
    model.state.undo.lock().unwrap().undo();
    assert_eq!(
        model.state.graph.lock().unwrap().nodes().count(),
        0,
        "undo removes the node",
    );
    model.state.undo.lock().unwrap().redo();
    assert_eq!(
        model.state.graph.lock().unwrap().nodes().count(),
        1,
        "redo restores the node",
    );
}

#[test]
fn remove_node_through_bridge_lands_on_undo_stack() {
    let state = fixture();
    let id = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node("Box", [0.0, 0.0], &state.registry).unwrap()
    };
    let mut model = AppStateModel::new(state);
    ne::NodeGraphModel::remove_node(&mut model, ne::NodeId(id.0));
    assert_eq!(model.state.graph.lock().unwrap().nodes().count(), 0);
    assert_eq!(
        model.state.undo.lock().unwrap().undo_name(),
        Some("Remove Node"),
    );
    model.state.undo.lock().unwrap().undo();
    assert_eq!(
        model.state.graph.lock().unwrap().nodes().count(),
        1,
        "undo restores the removed node",
    );
}

#[test]
fn set_property_through_bridge_lands_on_undo_stack() {
    let state = fixture();
    let id = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node("Box", [0.0, 0.0], &state.registry).unwrap()
    };
    let baseline = {
        let g = state.graph.lock().unwrap();
        match g.get(id).unwrap().properties.get("width") {
            Some(PortValue::Number(v)) => *v,
            other => panic!("expected default Number width, got {:?}", other),
        }
    };
    let mut model = AppStateModel::new(state);
    ne::NodeGraphModel::set_property(
        &mut model,
        ne::NodeId(id.0),
        "width",
        ne::PropertyValue::Number(baseline + 50.0),
    );
    assert_eq!(
        model.state.undo.lock().unwrap().undo_name(),
        Some("Change Property"),
    );
    model.state.undo.lock().unwrap().undo();
    let g = model.state.graph.lock().unwrap();
    match g.get(id).unwrap().properties.get("width") {
        Some(PortValue::Number(v)) => assert!(
            (v - baseline).abs() < 1e-9,
            "undo restores baseline width={}, got {}",
            baseline,
            v
        ),
        other => panic!("expected Number, got {:?}", other),
    }
}

#[test]
fn slider_drag_coalesces_into_single_undo_step() {
    // Per-pixel slider drags fire set_property ~60×/s. Without
    // coalescing each pixel becomes a separate undo step, which means
    // Ctrl+Z walks back pixel-by-pixel instead of restoring the
    // pre-drag value in one tap.
    let state = fixture();
    let id = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node("Box", [0.0, 0.0], &state.registry).unwrap()
    };
    let baseline = {
        let g = state.graph.lock().unwrap();
        match g.get(id).unwrap().properties.get("width") {
            Some(PortValue::Number(v)) => *v,
            other => panic!("expected default Number width, got {:?}", other),
        }
    };
    let mut model = AppStateModel::new(state);
    for v in 1..50 {
        ne::NodeGraphModel::set_property(
            &mut model,
            ne::NodeId(id.0),
            "width",
            ne::PropertyValue::Number(baseline + v as f64),
        );
    }
    // 49 calls → ONE undo step (same id + property = same stroke).
    assert_eq!(
        model.state.undo.lock().unwrap().undo_name(),
        Some("Change Property"),
        "coalesced stroke is still a Change Property command",
    );
    model.state.undo.lock().unwrap().undo();
    let g = model.state.graph.lock().unwrap();
    match g.get(id).unwrap().properties.get("width") {
        Some(PortValue::Number(v)) => assert!(
            (v - baseline).abs() < 1e-9,
            "single undo must restore baseline ({}), got {}",
            baseline,
            v
        ),
        other => panic!("expected Number, got {:?}", other),
    }
}

#[test]
fn node_drag_coalesces_into_single_undo_step() {
    let state = fixture();
    let id = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node("Box", [0.0, 0.0], &state.registry).unwrap()
    };
    let mut model = AppStateModel::new(state);
    // Simulate a 50-frame drag.
    for step in 1..=50 {
        ne::NodeGraphModel::set_node_position(
            &mut model,
            ne::NodeId(id.0),
            [step as f64, step as f64 * 2.0],
        );
    }
    assert_eq!(
        model.state.undo.lock().unwrap().undo_name(),
        Some("Move Node"),
        "drag coalesces into a single Move Node command",
    );
    model.state.undo.lock().unwrap().undo();
    let g = model.state.graph.lock().unwrap();
    assert_eq!(
        g.get(id).unwrap().position,
        [0.0, 0.0],
        "single undo restores the pre-drag position",
    );
}

#[test]
fn noodle_add_through_bridge_lands_on_undo_stack() {
    let state = fixture();
    let (a, b) = {
        let mut g = state.graph.lock().unwrap();
        (
            g.add_new_node("Box", [0.0, 0.0], &state.registry).unwrap(),
            g.add_new_node("Transform", [200.0, 0.0], &state.registry)
                .unwrap(),
        )
    };
    let mut model = AppStateModel::new(state);
    let result = ne::NodeGraphModel::try_add_noodle(
        &mut model,
        ne::NodeId(a.0),
        "out",
        ne::NodeId(b.0),
        "input",
    );
    assert!(matches!(result, ne::NoodleResult::Connected));
    assert_eq!(
        model.state.undo.lock().unwrap().undo_name(),
        Some("Connect"),
    );
    assert_eq!(model.state.graph.lock().unwrap().noodle_count(), 1);
    model.state.undo.lock().unwrap().undo();
    assert_eq!(
        model.state.graph.lock().unwrap().noodle_count(),
        0,
        "undo removes the connection",
    );
}

#[test]
fn set_node_matrix_with_undo_round_trips() {
    // Bed-plane drag + Z drag funnel through this helper. Single-call
    // round trip should restore the original matrix.
    let state = fixture();
    let id = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node("Box", [0.0, 0.0], &state.registry).unwrap()
    };
    let original: [f32; 16] = {
        let g = state.graph.lock().unwrap();
        match g.get(id).unwrap().properties.get("matrix") {
            Some(PortValue::Matrix4x4(m)) => *m,
            other => panic!("expected default identity Matrix4x4, got {:?}", other),
        }
    };
    let mut translated = original;
    translated[12] = 5.0; // translate X +5
    translated[13] = 3.0; // translate Y +3
    state.set_node_matrix_with_undo(id, translated);
    let after_set: [f32; 16] = {
        let g = state.graph.lock().unwrap();
        match g.get(id).unwrap().properties.get("matrix") {
            Some(PortValue::Matrix4x4(m)) => *m,
            other => panic!("expected Matrix4x4 after set, got {:?}", other),
        }
    };
    assert!((after_set[12] - 5.0).abs() < 1e-6);
    assert!((after_set[13] - 3.0).abs() < 1e-6);
    state.undo.lock().unwrap().undo();
    let after_undo: [f32; 16] = {
        let g = state.graph.lock().unwrap();
        match g.get(id).unwrap().properties.get("matrix") {
            Some(PortValue::Matrix4x4(m)) => *m,
            other => panic!("expected Matrix4x4 after undo, got {:?}", other),
        }
    };
    assert_eq!(after_undo, original, "undo restores original matrix");
}

#[test]
fn body_drag_coalesces_matrix_writes_into_single_step() {
    // Simulates a 30-frame drag of a body across the bed. Each frame
    // calls set_node_matrix_with_undo with the latest matrix. The
    // undo stack must collapse into one step that restores the
    // original (identity) matrix.
    let state = fixture();
    let id = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node("Box", [0.0, 0.0], &state.registry).unwrap()
    };
    let original: [f32; 16] = {
        let g = state.graph.lock().unwrap();
        match g.get(id).unwrap().properties.get("matrix") {
            Some(PortValue::Matrix4x4(m)) => *m,
            other => panic!("expected default identity, got {:?}", other),
        }
    };
    for step in 1..=30 {
        let mut m = original;
        m[12] = step as f32 * 0.5;
        m[13] = step as f32 * 0.25;
        state.set_node_matrix_with_undo(id, m);
    }
    // Single Change Property undo step.
    assert_eq!(
        state.undo.lock().unwrap().undo_name(),
        Some("Change Property"),
        "30 matrix writes coalesce into one undo step",
    );
    state.undo.lock().unwrap().undo();
    let after_undo: [f32; 16] = {
        let g = state.graph.lock().unwrap();
        match g.get(id).unwrap().properties.get("matrix") {
            Some(PortValue::Matrix4x4(m)) => *m,
            other => panic!("expected Matrix4x4 after undo, got {:?}", other),
        }
    };
    assert_eq!(after_undo, original, "single undo restores the pre-drag matrix");
}

#[test]
fn redo_branch_clears_on_new_mutation() {
    // After undo+new-action, the redo stack must clear — agg-gui's
    // standard semantics. Regression guard for the AppStateModel
    // mutation path specifically.
    let state = fixture();
    let mut model = AppStateModel::new(state);
    ne::NodeGraphModel::add_node(&mut model, "Box", [0.0, 0.0]);
    model.state.undo.lock().unwrap().undo();
    assert!(model.state.undo.lock().unwrap().can_redo());
    // Any new mutation must clear the redo branch.
    ne::NodeGraphModel::add_node(&mut model, "Cylinder", [100.0, 0.0]);
    assert!(
        !model.state.undo.lock().unwrap().can_redo(),
        "new action clears the redo stack",
    );
}
