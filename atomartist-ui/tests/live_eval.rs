//! Integration tests for Phase 4 live evaluation.

use std::sync::Arc;

use atomartist_lib::graph::node::PortValue;
use atomartist_lib::nodes;
use atomartist_lib::registry::NodeRegistry;
use atomartist_lib::Graph;
use atomartist_ui::AppState;
use atomartist_ui::canvas_widget::add_node_with_defaults;

fn fresh_state() -> AppState {
    let mut reg = NodeRegistry::new();
    nodes::register_all(&mut reg);
    AppState::new(Graph::new(), reg)
}

#[test]
fn evaluate_now_populates_last_mesh_for_box() {
    let state = fresh_state();
    let id = {
        let mut g = state.graph.lock().unwrap();
        add_node_with_defaults(&mut g, &state.registry, "Box", [0.0, 0.0]).unwrap()
    };
    state.set_display_node(Some(id));
    state.evaluate_now();
    let mesh = state.last_mesh_output.lock().unwrap().clone();
    assert!(mesh.is_some(), "expected last_mesh_output to be populated");
    let mesh = mesh.unwrap();
    let n_verts = mesh.vert_properties.len() / mesh.num_prop as usize;
    assert_eq!(n_verts, 24);
}

#[test]
fn property_change_then_evaluate_yields_different_mesh() {
    let state = fresh_state();
    let id = {
        let mut g = state.graph.lock().unwrap();
        add_node_with_defaults(&mut g, &state.registry, "Box", [0.0, 0.0]).unwrap()
    };
    state.set_display_node(Some(id));
    state.evaluate_now();
    let mesh_a = state.last_mesh_output.lock().unwrap().clone().unwrap();

    // Mutate width and re-evaluate.
    {
        let mut g = state.graph.lock().unwrap();
        g.set_property(id, "width", PortValue::Number(5.0)).unwrap();
    }
    state.evaluate_now();
    let mesh_b = state.last_mesh_output.lock().unwrap().clone().unwrap();

    // Same vertex/triangle counts, different vertex coords.
    assert_eq!(mesh_a.vert_properties.len(), mesh_b.vert_properties.len());
    let mut differs = false;
    for i in 0..mesh_a.vert_properties.len() {
        if (mesh_a.vert_properties[i] - mesh_b.vert_properties[i]).abs() > 1e-5 {
            differs = true;
            break;
        }
    }
    assert!(differs, "mesh after width change should differ from before");
    // Specifically: max X of mesh_b should be 5/2 = 2.5
    let mut max_x = f32::NEG_INFINITY;
    let stride = mesh_b.num_prop as usize;
    for i in 0..mesh_b.vert_properties.len() / stride {
        let x = mesh_b.vert_properties[i * stride];
        if x > max_x { max_x = x; }
    }
    assert!((max_x - 2.5).abs() < 1e-5, "max x should be 2.5, was {}", max_x);
    // Sanity drop on the Arc clone.
    drop(Arc::clone(&mesh_a));
}

#[test]
fn schedule_evaluate_eventually_populates_last_mesh() {
    // This test uses the synchronous evaluate_now to keep the test
    // deterministic on both native and WASM. schedule_evaluate is
    // exercised in interactive widget tests.
    let state = fresh_state();
    {
        let mut g = state.graph.lock().unwrap();
        add_node_with_defaults(&mut g, &state.registry, "Cylinder", [0.0, 0.0]).unwrap();
    }
    state.evaluate_now();
    assert!(state.last_mesh_output.lock().unwrap().is_some());
    assert!(state.take_viewport_dirty(), "viewport_dirty should be set after eval");
    assert!(!state.take_viewport_dirty(), "second take should clear");
}
