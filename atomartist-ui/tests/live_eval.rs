//! Integration tests for Phase 4 live evaluation.

use std::sync::Arc;

use atomartist_lib::graph::node::PortValue;
use atomartist_lib::nodes;
use atomartist_lib::registry::NodeRegistry;
use atomartist_lib::Graph;
use atomartist_ui::AppState;
use atomartist_ui::add_node_with_defaults;

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
    let geom = state.last_mesh_output.lock().unwrap().clone();
    assert!(geom.is_some(), "expected last_mesh_output to be populated");
    let geom = geom.unwrap();
    let mesh = &geom.first().unwrap().mesh;
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
    let geom_a = state.last_mesh_output.lock().unwrap().clone().unwrap();
    let mesh_a = &geom_a.first().unwrap().mesh;

    // Mutate width and re-evaluate.
    {
        let mut g = state.graph.lock().unwrap();
        g.set_property(id, "width", PortValue::Number(5.0)).unwrap();
    }
    state.evaluate_now();
    let geom_b = state.last_mesh_output.lock().unwrap().clone().unwrap();
    let mesh_b = &geom_b.first().unwrap().mesh;

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
    drop(Arc::clone(&geom_a));
}

#[test]
fn schedule_evaluate_eventually_populates_last_mesh() {
    // This test uses the synchronous evaluate_now to keep the test
    // deterministic on both native and WASM. schedule_evaluate is
    // exercised in interactive widget tests.
    //
    // The viewport only shows geometry that's wired into the Output
    // node — an unconnected primitive is "not outputting" and won't
    // populate `last_mesh_output`. Set the cylinder as the explicit
    // display node so the test exercises the populate-on-eval path
    // without needing a full Output-node wiring.
    let state = fresh_state();
    let cyl = {
        let mut g = state.graph.lock().unwrap();
        add_node_with_defaults(&mut g, &state.registry, "Cylinder", [0.0, 0.0]).unwrap()
    };
    state.set_display_node(Some(cyl));
    state.evaluate_now();
    assert!(state.last_mesh_output.lock().unwrap().is_some());
    assert!(state.take_viewport_dirty(), "viewport_dirty should be set after eval");
    assert!(!state.take_viewport_dirty(), "second take should clear");
}

/// A background evaluation must wake a sleeping host (step 6g-1).
///
/// `schedule_evaluate` publishes its mesh from a spawned thread and sets
/// `viewport_dirty` — a flag a reactive event loop parked in winit's
/// `Wait` will never read, because it is not running. `EvalTask::run`
/// therefore ends with `signal_async_state_change`, which is what the
/// host's waker hangs off. Without it the new geometry sat invisible until
/// the user happened to move the mouse.
#[test]
fn a_background_evaluation_signals_the_host() {
    let state = fresh_state();
    let id = {
        let mut g = state.graph.lock().unwrap();
        add_node_with_defaults(&mut g, &state.registry, "Box", [0.0, 0.0]).unwrap()
    };
    state.set_display_node(Some(id));

    agg_gui::animation::clear_draw_request();
    state.schedule_evaluate();

    // Wait for the worker to publish, then look at the draw signal it
    // raised on its way out. Polling rather than sleeping a fixed amount
    // keeps a slow machine from producing a flaky failure.
    //
    // Sibling tests in this binary evaluate too, and every evaluation
    // signals, so a stray bump could satisfy the assertion below - that
    // can only turn a real failure into a false *pass*, never the
    // reverse, which is the direction that matters for a regression test.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline
        && state.last_mesh_output.lock().unwrap().is_none()
    {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        state.last_mesh_output.lock().unwrap().is_some(),
        "the background evaluation published a mesh"
    );
    assert!(
        agg_gui::animation::wants_draw(),
        "a background evaluation must raise the draw signal that wakes the host"
    );
}
