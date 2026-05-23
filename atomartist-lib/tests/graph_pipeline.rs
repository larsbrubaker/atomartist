//! End-to-end integration test: build a graph using only public API,
//! evaluate it, and verify the geometry that flows out the back.

use std::sync::Arc;

use atomartist_lib::{
    graph::{executor::evaluate_all, Noodle, Graph, NodeId, PortValue},
    nodes,
    registry::NodeRegistry,
};
use manifold_rust::types::MeshGL;

fn box_transform_graph() -> (Graph, NodeRegistry, NodeId) {
    let mut reg = NodeRegistry::new();
    nodes::register_all(&mut reg);
    let mut g = Graph::new();

    let box_id = g.add_new_node("Box", [0.0, 0.0], &reg).unwrap();
    g.set_property(box_id, "width", PortValue::Number(2.0)).unwrap();
    g.set_property(box_id, "height", PortValue::Number(2.0)).unwrap();
    g.set_property(box_id, "depth", PortValue::Number(2.0)).unwrap();

    let xform_id = g.add_new_node("Transform", [200.0, 0.0], &reg).unwrap();
    g.set_property(xform_id, "ty", PortValue::Number(5.0)).unwrap();
    g.set_property(xform_id, "sx", PortValue::Number(1.0)).unwrap();
    g.set_property(xform_id, "sy", PortValue::Number(1.0)).unwrap();
    g.set_property(xform_id, "sz", PortValue::Number(1.0)).unwrap();

    let out_box = g.get(box_id).unwrap().output_by_name("out").unwrap().uid;
    let in_xform = g.get(xform_id).unwrap().input_by_name("input").unwrap().uid;
    g.connect(Noodle::new(box_id, out_box, xform_id, in_xform), &reg).unwrap();

    (g, reg, xform_id)
}

#[test]
fn box_through_transform_produces_translated_mesh() {
    let (mut g, reg, xform_id) = box_transform_graph();
    evaluate_all(&mut g, &reg).unwrap();

    let out_uid = g.get(xform_id).unwrap().output_by_name("out").unwrap().uid;
    let out = g.get(xform_id).unwrap()
        .cached_outputs.get(&out_uid).cloned().unwrap();
    match out {
        PortValue::Geometry3d(mesh) => {
            assert_mesh_translated_y(&mesh, 5.0);
        }
        _ => panic!("expected Geometry3d output"),
    }
}

fn assert_mesh_translated_y(mesh: &MeshGL, expected_dy: f32) {
    let stride = mesh.num_prop as usize;
    assert!(stride > 0);
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    let n = mesh.vert_properties.len() / stride;
    for i in 0..n {
        let y = mesh.vert_properties[i * stride + 1];
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    assert!((min_y - (expected_dy - 1.0)).abs() < 1e-5,
            "min_y was {}, expected {}", min_y, expected_dy - 1.0);
    assert!((max_y - (expected_dy + 1.0)).abs() < 1e-5,
            "max_y was {}, expected {}", max_y, expected_dy + 1.0);
}

#[test]
fn registry_has_expected_phase2_nodes() {
    let mut reg = NodeRegistry::new();
    nodes::register_all(&mut reg);
    assert!(reg.get("Box").is_some());
    assert!(reg.get("Cylinder").is_some());
    assert!(reg.get("Sphere").is_some());
    assert!(reg.get("Transform").is_some());
    assert!(reg.get("Combine").is_some());
}

#[test]
fn rectangle_through_extrude_produces_solid() {
    let mut reg = NodeRegistry::new();
    nodes::register_all(&mut reg);
    let mut g = Graph::new();
    let r = g.add_new_node("Rectangle", [0.0, 0.0], &reg).unwrap();
    g.set_property(r, "width", PortValue::Number(4.0)).unwrap();
    g.set_property(r, "height", PortValue::Number(2.0)).unwrap();
    let e = g.add_new_node("Extrude", [200.0, 0.0], &reg).unwrap();
    g.set_property(e, "height", PortValue::Number(3.0)).unwrap();
    let out_r = g.get(r).unwrap().output_by_name("out").unwrap().uid;
    let in_e = g.get(e).unwrap().input_by_name("Paths").unwrap().uid;
    g.connect(Noodle::new(r, out_r, e, in_e), &reg).unwrap();

    atomartist_lib::graph::executor::evaluate_all(&mut g, &reg).unwrap();
    let geo_uid = g.get(e).unwrap().output_by_name("Geometry").unwrap().uid;
    match g.get(e).unwrap().cached_outputs.get(&geo_uid) {
        Some(PortValue::Geometry3d(m)) => {
            assert!(m.vert_properties.len() > 0);
            assert!(m.tri_verts.len() >= 12);
            let stride = m.num_prop as usize;
            let n = m.vert_properties.len() / stride;
            let mut z_min = f32::INFINITY; let mut z_max = f32::NEG_INFINITY;
            for i in 0..n {
                let z = m.vert_properties[i * stride + 2];
                if z < z_min { z_min = z; }
                if z > z_max { z_max = z; }
            }
            assert!((z_min + 1.5).abs() < 1e-4);
            assert!((z_max - 1.5).abs() < 1e-4);
        }
        _ => panic!("expected Geometry3d on Extrude.Geometry"),
    }
}

#[test]
fn combine_two_boxes_via_executor() {
    let mut reg = NodeRegistry::new();
    nodes::register_all(&mut reg);
    let mut g = Graph::new();

    let a = g.add_new_node("Box", [0.0, 0.0], &reg).unwrap();
    let b = g.add_new_node("Box", [0.0, 100.0], &reg).unwrap();
    let c = g.add_new_node("Combine", [200.0, 0.0], &reg).unwrap();

    // Combine uses the dynamic-input model now (first connection lands
    // on the trailing empty slot; a fresh empty slot follows). The two
    // wires from Box "out" sockets land in the first then second slot.
    let out_a = g.get(a).unwrap().output_by_name("out").unwrap().uid;
    let out_b = g.get(b).unwrap().output_by_name("out").unwrap().uid;
    let slot_1 = g.get(c).unwrap().inputs[0].uid;
    g.connect(Noodle::new(a, out_a, c, slot_1), &reg).unwrap();
    let slot_2 = g.get(c).unwrap().inputs.last().unwrap().uid;
    g.connect(Noodle::new(b, out_b, c, slot_2), &reg).unwrap();

    evaluate_all(&mut g, &reg).unwrap();
    let out_c = g.get(c).unwrap().output_by_name("out").unwrap().uid;
    match g.get(c).unwrap().cached_outputs.get(&out_c) {
        Some(PortValue::Geometry3d(m)) => {
            assert_eq!(m.vert_properties.len() / m.num_prop as usize, 48);
            assert_eq!(m.tri_verts.len() / 3, 24);
        }
        other => panic!("unexpected output: {:?}", other.map(|v| v.socket_type())),
    }

    let _arc: Arc<MeshGL> = match g.get(c).unwrap().cached_outputs.get(&out_c) {
        Some(PortValue::Geometry3d(m)) => m.clone(),
        _ => panic!(),
    };
}
