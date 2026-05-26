//! Ported from NodeDesigner's `tests/unit/matrix-utils.test.ts`.
//!
//! Verifies that AtomArtist's column-major 4x4 matrix transforms
//! produce the expected results when applied to a Box mesh through
//! the Transform node — round-tripping rotation + translation + scale
//! ought to round-trip vertices to their pre-transform positions
//! within float epsilon.
//!
//! Note: as of the matrix-composition rewrite, the Transform node
//! stores its transform on `Body.matrix` instead of baking into
//! vertices. These tests apply `Body.matrix` to `Body.mesh` at the
//! assertion boundary (`world_mesh()` helper) so they continue to
//! check the user-visible composite effect rather than testing the
//! old mesh-baking implementation detail.

use std::sync::Arc;

use atomartist_lib::geometry::{apply_transform, generate_box, get_pos, num_verts};
use atomartist_lib::graph::node::{NodeId, NodeInstance, PortValue};
use atomartist_lib::graph::socket::SocketUidAlloc;
use atomartist_lib::nodes::ops_3d::transform_node::TransformNode;
use atomartist_lib::registry::{EvalCtx, NodeDef, NodeInputs, NodeProperties};

fn fixture(
    mesh: Arc<manifold_rust::types::MeshGL>,
    props_kv: &[(&'static str, f64)],
) -> (NodeInstance, NodeInputs, NodeProperties) {
    let mut alloc = SocketUidAlloc::new();
    let tpl = TransformNode.instantiate(&mut alloc);
    let mut inst = NodeInstance::new(NodeId(1), "Transform", [0.0, 0.0]);
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
        props.insert(*k, PortValue::Number(*v));
    }
    // Resolution defaults so compose_with_upstream doesn't panic.
    props.insert("color", PortValue::Color(atomartist_lib::geometry::INHERIT_COLOR));
    props.insert("matrix", PortValue::Matrix4x4(
        atomartist_lib::graph::node::identity_matrix(),
    ));
    (inst, inputs, props)
}

/// Apply `body.matrix` to `body.mesh` so we can check world-space
/// positions after the Transform node has run. With the matrix-
/// composition rewrite, the Body's mesh is shared with the upstream
/// and only the matrix changes — but the user-visible effect is the
/// matrix applied to the mesh, which `apply_transform` realises.
fn world_mesh(body: &atomartist_lib::geometry::Body) -> manifold_rust::types::MeshGL {
    apply_transform(&body.mesh, &body.matrix)
}

#[test]
fn identity_transform_preserves_positions() {
    let m = Arc::new(generate_box(2.0, 3.0, 4.0));
    let (inst, inputs, props) = fixture(m.clone(), &[("sx", 1.0), ("sy", 1.0), ("sz", 1.0)]);
    let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
    let outs = TransformNode.evaluate(&ctx).unwrap();
    match outs.by_name.get("out").unwrap() {
        PortValue::Geometry3d(t) => {
            let world = world_mesh(t.first().unwrap());
            for i in 0..num_verts(&world) {
                let p = get_pos(&world, i);
                let p0 = get_pos(&m, i);
                for k in 0..3 {
                    assert!(
                        (p[k] - p0[k]).abs() < 1e-5,
                        "vert {} axis {} differs: {} vs {}",
                        i, k, p[k], p0[k]
                    );
                }
            }
        }
        _ => panic!("expected Geometry3d"),
    }
}

#[test]
fn nonuniform_scale_changes_each_axis_independently() {
    let m = Arc::new(generate_box(2.0, 2.0, 2.0));
    let (inst, inputs, props) = fixture(m.clone(), &[("sx", 2.0), ("sy", 0.5), ("sz", 1.0)]);
    let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
    let outs = TransformNode.evaluate(&ctx).unwrap();
    match outs.by_name.get("out").unwrap() {
        PortValue::Geometry3d(t) => {
            let world = world_mesh(t.first().unwrap());
            for i in 0..num_verts(&world) {
                let p = get_pos(&world, i);
                let p0 = get_pos(&m, i);
                assert!((p[0] - p0[0] * 2.0).abs() < 1e-5);
                assert!((p[1] - p0[1] * 0.5).abs() < 1e-5);
                assert!((p[2] - p0[2]).abs() < 1e-5);
            }
        }
        _ => panic!(),
    }
}

#[test]
fn rotation_z_180_then_again_round_trips() {
    let m = generate_box(2.0, 3.0, 4.0);
    use std::f32::consts::PI;
    let a = PI;
    let c = a.cos();
    let s = a.sin();
    let rot_z: [f32; 16] = [
        c, s, 0.0, 0.0,
       -s, c, 0.0, 0.0,
        0.0, 0.0, 1.0, 0.0,
        0.0, 0.0, 0.0, 1.0,
    ];
    let once = apply_transform(&m, &rot_z);
    let twice = apply_transform(&once, &rot_z);
    for i in 0..num_verts(&m) {
        let p = get_pos(&twice, i);
        let p0 = get_pos(&m, i);
        for k in 0..3 {
            assert!(
                (p[k] - p0[k]).abs() < 1e-4,
                "vert {} axis {} differs after double rotation: {} vs {}",
                i, k, p[k], p0[k]
            );
        }
    }
}

#[test]
fn translation_shifts_origin() {
    let m = Arc::new(generate_box(1.0, 1.0, 1.0));
    let (inst, inputs, props) = fixture(
        m.clone(),
        &[
            ("tx", 5.0),
            ("ty", -3.0),
            ("tz", 7.0),
            ("sx", 1.0), ("sy", 1.0), ("sz", 1.0),
        ],
    );
    let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
    let outs = TransformNode.evaluate(&ctx).unwrap();
    match outs.by_name.get("out").unwrap() {
        PortValue::Geometry3d(t) => {
            let world = world_mesh(t.first().unwrap());
            let mut sum = [0.0f32; 3];
            for i in 0..num_verts(&world) {
                let p = get_pos(&world, i);
                for k in 0..3 { sum[k] += p[k]; }
            }
            let n = num_verts(&world) as f32;
            assert!((sum[0] / n - 5.0).abs() < 1e-4);
            assert!((sum[1] / n - (-3.0)).abs() < 1e-4);
            assert!((sum[2] / n - 7.0).abs() < 1e-4);
        }
        _ => panic!(),
    }
}
