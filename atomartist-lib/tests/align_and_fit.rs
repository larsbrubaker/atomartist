//! Ported from NodeDesigner's `tests/unit/nodes-align.test.ts` and
//! `tests/unit/nodes-fit-to-bounds.test.ts`.

use std::sync::Arc;

use atomartist_lib::geometry::{bounds, generate_box};
use atomartist_lib::graph::node::PortValue;
use atomartist_lib::nodes::ops_3d::align_node::AlignNode;
use atomartist_lib::nodes::ops_3d::fit_to_bounds_node::FitToBoundsNode;
use atomartist_lib::registry::{NodeDef, NodeInputs, NodeProperties};

fn input_with(mesh: Arc<manifold_rust::types::MeshGL>) -> NodeInputs {
    let mut i = NodeInputs::default();
    i.insert("input", PortValue::Geometry3d(mesh));
    i
}

fn props(values: &[(&'static str, f64)]) -> NodeProperties {
    let mut p = NodeProperties::default();
    for (k, v) in values {
        p.insert(k, PortValue::Number(*v));
    }
    p
}

#[test]
fn align_default_sits_on_floor_plane_centered() {
    // Default: align_x=0, align_y=-1 (min Y → 0), align_z=0.
    // Source: 4×6×8 box centered at origin → bounds (-2,-3,-4)..(2,3,4).
    // Result: Y min should become 0, X and Z should remain centered.
    let m = Arc::new(generate_box(4.0, 6.0, 8.0));
    let outs = AlignNode.evaluate(&input_with(m), &props(&[])).unwrap();
    match outs.by_name.get("out").unwrap() {
        PortValue::Geometry3d(t) => {
            let (mn, mx) = bounds(t).unwrap();
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
    // align_y = +1 pulls the model down so its top sits at Y=0.
    let m = Arc::new(generate_box(2.0, 4.0, 2.0));
    let outs = AlignNode.evaluate(
        &input_with(m),
        &props(&[("align_y", 1.0)]),
    ).unwrap();
    match outs.by_name.get("out").unwrap() {
        PortValue::Geometry3d(t) => {
            let (_, mx) = bounds(t).unwrap();
            assert!((mx[1] - 0.0).abs() < 1e-4, "y_max should be 0, got {}", mx[1]);
        }
        _ => panic!(),
    }
}

#[test]
fn fit_to_bounds_uniform_keeps_aspect() {
    // Box 4×2×8 (aspect 2:1:4) fit into 10×10×10 → uniformly scaled
    // by 10/8 = 1.25, so dimensions become 5×2.5×10.
    let m = Arc::new(generate_box(4.0, 2.0, 8.0));
    let mut p = props(&[("width", 10.0), ("height", 10.0), ("depth", 10.0)]);
    p.insert("uniform", PortValue::Bool(true));
    let outs = FitToBoundsNode.evaluate(&input_with(m), &p).unwrap();
    match outs.by_name.get("out").unwrap() {
        PortValue::Geometry3d(t) => {
            let (mn, mx) = bounds(t).unwrap();
            let dx = mx[0] - mn[0];
            let dy = mx[1] - mn[1];
            let dz = mx[2] - mn[2];
            assert!((dx - 5.0).abs() < 1e-3);
            assert!((dy - 2.5).abs() < 1e-3);
            assert!((dz - 10.0).abs() < 1e-3);
        }
        _ => panic!(),
    }
}

#[test]
fn fit_to_bounds_stretch_fills_each_axis() {
    let m = Arc::new(generate_box(4.0, 2.0, 8.0));
    let mut p = props(&[("width", 10.0), ("height", 10.0), ("depth", 10.0)]);
    p.insert("uniform", PortValue::Bool(false));
    let outs = FitToBoundsNode.evaluate(&input_with(m), &p).unwrap();
    match outs.by_name.get("out").unwrap() {
        PortValue::Geometry3d(t) => {
            let (mn, mx) = bounds(t).unwrap();
            assert!((mx[0] - mn[0] - 10.0).abs() < 1e-3);
            assert!((mx[1] - mn[1] - 10.0).abs() < 1e-3);
            assert!((mx[2] - mn[2] - 10.0).abs() < 1e-3);
        }
        _ => panic!(),
    }
}
