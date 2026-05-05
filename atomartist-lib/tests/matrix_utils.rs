//! Ported from NodeDesigner's `tests/unit/matrix-utils.test.ts`.
//!
//! Verifies that AtomArtist's column-major 4x4 matrix transforms
//! produce the expected results when applied to a Box mesh through
//! the Transform node — round-tripping rotation + translation + scale
//! ought to round-trip vertices to their pre-transform positions
//! within float epsilon.

use std::sync::Arc;

use atomartist_lib::geometry::{apply_transform, generate_box, get_pos, num_verts};
use atomartist_lib::graph::node::PortValue;
use atomartist_lib::nodes::ops_3d::transform_node::TransformNode;
use atomartist_lib::registry::{NodeDef, NodeInputs, NodeProperties};

fn props_with(values: &[(&'static str, f64)]) -> NodeProperties {
    let mut p = NodeProperties::default();
    for (k, v) in values {
        p.insert(k, PortValue::Number(*v));
    }
    p
}

fn input_with(mesh: Arc<manifold_rust::types::MeshGL>) -> NodeInputs {
    let mut i = NodeInputs::default();
    i.insert("input", PortValue::Geometry3d(mesh));
    i
}

#[test]
fn identity_transform_preserves_positions() {
    let m = Arc::new(generate_box(2.0, 3.0, 4.0));
    let outs = TransformNode.evaluate(
        &input_with(m.clone()),
        &props_with(&[("sx", 1.0), ("sy", 1.0), ("sz", 1.0)]),
    ).unwrap();
    match outs.by_name.get("out").unwrap() {
        PortValue::Geometry3d(t) => {
            for i in 0..num_verts(t) {
                let p = get_pos(t, i);
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
    let outs = TransformNode.evaluate(
        &input_with(m.clone()),
        &props_with(&[("sx", 2.0), ("sy", 0.5), ("sz", 1.0)]),
    ).unwrap();
    match outs.by_name.get("out").unwrap() {
        PortValue::Geometry3d(t) => {
            for i in 0..num_verts(t) {
                let p = get_pos(t, i);
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
    // Two 180° rotations around Z = identity.
    // Build the matrix manually and apply twice — sanity check on
    // apply_transform's column-major math.
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
    let outs = TransformNode.evaluate(
        &input_with(m.clone()),
        &props_with(&[
            ("tx", 5.0),
            ("ty", -3.0),
            ("tz", 7.0),
            ("sx", 1.0), ("sy", 1.0), ("sz", 1.0),
        ]),
    ).unwrap();
    match outs.by_name.get("out").unwrap() {
        PortValue::Geometry3d(t) => {
            // Box is centered at origin; after translate, center is (5, -3, 7).
            let mut sum = [0.0f32; 3];
            for i in 0..num_verts(t) {
                let p = get_pos(t, i);
                for k in 0..3 { sum[k] += p[k]; }
            }
            let n = num_verts(t) as f32;
            assert!((sum[0] / n - 5.0).abs() < 1e-4);
            assert!((sum[1] / n - (-3.0)).abs() < 1e-4);
            assert!((sum[2] / n - 7.0).abs() < 1e-4);
        }
        _ => panic!(),
    }
}
