//! Ported from NodeDesigner's `tests/unit/nodes-align.test.ts` and
//! `tests/unit/nodes-fit-to-bounds.test.ts`.

use std::sync::Arc;

use atomartist_lib::geometry::{apply_transform, bounds, generate_box, Body};
use atomartist_lib::graph::node::{NodeId, NodeInstance, PortValue};
use atomartist_lib::graph::socket::SocketUidAlloc;
use atomartist_lib::nodes::ops_3d::align_node::AlignNode;
use atomartist_lib::nodes::ops_3d::fit_to_bounds_node::FitToBoundsNode;
use atomartist_lib::registry::{EvalCtx, NodeDef, NodeInputs, NodeProperties};

/// Build (instance, NodeInputs, NodeProperties) for a node that has a
/// single `"input"` Geometry3d socket. Sufficient for the align/fit
/// tests; matches what `Graph::add_new_node` would produce.
fn fixture(
    node: &impl NodeDef,
    mesh: Arc<manifold_rust::types::MeshGL>,
    props_kv: &[(&'static str, PortValue)],
) -> (NodeInstance, NodeInputs, NodeProperties) {
    let mut alloc = SocketUidAlloc::new();
    let tpl = node.instantiate(&mut alloc);
    let mut inst = NodeInstance::new(NodeId(1), node.type_id().to_string(), [0.0, 0.0]);
    inst.inputs = tpl.inputs;
    inst.outputs = tpl.outputs;
    let mut inputs = NodeInputs::default();
    let uid = inst.input_by_name("input").unwrap().uid;
    inputs.insert(
        uid,
        PortValue::Geometry3d(Arc::new(
            atomartist_lib::geometry::Geometry3d::from_mesh(mesh),
        )),
    );
    let mut props = NodeProperties::default();
    for (k, v) in props_kv {
        props.insert(*k, v.clone());
    }
    // Defaults so matrix-composition ops can resolve colour + matrix.
    props.insert("color", PortValue::Color(atomartist_lib::geometry::INHERIT_COLOR));
    props.insert("matrix", PortValue::Matrix4x4(
        atomartist_lib::graph::node::identity_matrix(),
    ));
    (inst, inputs, props)
}

/// Bounds of a body's mesh after its matrix is applied. Matrix-
/// composition ops (Transform, FitToBounds) keep meshes in local
/// space, so local `bounds()` no longer reflects what the user sees.
fn world_bounds(body: &Body) -> Option<([f32; 3], [f32; 3])> {
    let world = apply_transform(&body.mesh, &body.matrix);
    bounds(&world)
}

#[test]
fn align_default_sits_on_floor_plane_centered() {
    let m = Arc::new(generate_box(4.0, 6.0, 8.0));
    let (inst, inputs, props) = fixture(&AlignNode, m, &[]);
    let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
    let outs = AlignNode.evaluate(&ctx).unwrap();
    match outs.by_name.get("out").unwrap() {
        PortValue::Geometry3d(t) => {
            // Default `align_y = -1` puts the Y-min on the floor; check
            // via world bounds since Align composes into Body.matrix.
            let (mn, mx) = world_bounds(t.first().unwrap()).unwrap();
            assert!((mn[1] - 0.0).abs() < 1e-4, "y_min should be 0, got {}", mn[1]);
            assert!((mx[1] - 6.0).abs() < 1e-4, "y_max should be 6, got {}", mx[1]);
            assert!(((mn[0] + mx[0]) * 0.5).abs() < 1e-4);
            assert!(((mn[2] + mx[2]) * 0.5).abs() < 1e-4);
        }
        _ => panic!("expected Geometry3d"),
    }
}

#[test]
fn align_max_y_puts_top_at_origin() {
    let m = Arc::new(generate_box(2.0, 4.0, 2.0));
    let (inst, inputs, props) = fixture(&AlignNode, m, &[("align_y", PortValue::Number(1.0))]);
    let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
    let outs = AlignNode.evaluate(&ctx).unwrap();
    match outs.by_name.get("out").unwrap() {
        PortValue::Geometry3d(t) => {
            let (_, mx) = world_bounds(t.first().unwrap()).unwrap();
            assert!((mx[1] - 0.0).abs() < 1e-4, "y_max should be 0, got {}", mx[1]);
        }
        _ => panic!(),
    }
}

#[test]
fn fit_to_bounds_uniform_keeps_aspect() {
    let m = Arc::new(generate_box(4.0, 2.0, 8.0));
    let (inst, inputs, props) = fixture(
        &FitToBoundsNode,
        m,
        &[
            ("width", PortValue::Number(10.0)),
            ("height", PortValue::Number(10.0)),
            ("depth", PortValue::Number(10.0)),
            ("uniform", PortValue::Bool(true)),
        ],
    );
    let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
    let outs = FitToBoundsNode.evaluate(&ctx).unwrap();
    match outs.by_name.get("out").unwrap() {
        PortValue::Geometry3d(t) => {
            // FitToBounds composes scale into Body.matrix; world bounds
            // are what the user sees.
            let (mn, mx) = world_bounds(t.first().unwrap()).unwrap();
            let dx = mx[0] - mn[0];
            let dy = mx[1] - mn[1];
            let dz = mx[2] - mn[2];
            assert!((dx - 5.0).abs() < 1e-3, "dx was {dx}, expected 5");
            assert!((dy - 2.5).abs() < 1e-3, "dy was {dy}, expected 2.5");
            assert!((dz - 10.0).abs() < 1e-3, "dz was {dz}, expected 10");
        }
        _ => panic!(),
    }
}

#[test]
fn fit_to_bounds_stretch_fills_each_axis() {
    let m = Arc::new(generate_box(4.0, 2.0, 8.0));
    let (inst, inputs, props) = fixture(
        &FitToBoundsNode,
        m,
        &[
            ("width", PortValue::Number(10.0)),
            ("height", PortValue::Number(10.0)),
            ("depth", PortValue::Number(10.0)),
            ("uniform", PortValue::Bool(false)),
        ],
    );
    let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
    let outs = FitToBoundsNode.evaluate(&ctx).unwrap();
    match outs.by_name.get("out").unwrap() {
        PortValue::Geometry3d(t) => {
            let (mn, mx) = world_bounds(t.first().unwrap()).unwrap();
            assert!((mx[0] - mn[0] - 10.0).abs() < 1e-3);
            assert!((mx[1] - mn[1] - 10.0).abs() < 1e-3);
            assert!((mx[2] - mn[2] - 10.0).abs() < 1e-3);
        }
        _ => panic!(),
    }
}
