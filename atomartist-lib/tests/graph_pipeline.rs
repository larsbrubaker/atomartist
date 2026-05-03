//! End-to-end integration test: build a graph using only public API,
//! evaluate it, and verify the geometry that flows out the back.

use std::sync::Arc;

use atomartist_lib::{
    graph::{
        executor::evaluate_all,
        Edge, Graph, NodeId, NodeInstance, PortValue, SocketId,
    },
    nodes,
    registry::NodeRegistry,
};
use manifold_rust::types::MeshGL;

fn box_transform_graph() -> (Graph, NodeRegistry, NodeId) {
    let mut reg = NodeRegistry::new();
    nodes::register_all(&mut reg);
    let mut g = Graph::new();

    let box_id = g.allocate_id();
    let mut box_node = NodeInstance::new(box_id, "Box", [0.0, 0.0]);
    box_node.properties.insert("width", PortValue::Number(2.0));
    box_node.properties.insert("height", PortValue::Number(2.0));
    box_node.properties.insert("depth", PortValue::Number(2.0));
    g.add_node(box_node).unwrap();

    let xform_id = g.allocate_id();
    let mut xform = NodeInstance::new(xform_id, "Transform", [200.0, 0.0]);
    xform.properties.insert("ty", PortValue::Number(5.0));
    xform.properties.insert("sx", PortValue::Number(1.0));
    xform.properties.insert("sy", PortValue::Number(1.0));
    xform.properties.insert("sz", PortValue::Number(1.0));
    g.add_node(xform).unwrap();

    g.connect(
        Edge {
            from: SocketId { node: box_id, name: "out" },
            to: SocketId { node: xform_id, name: "input" },
        },
        &reg,
    )
    .unwrap();

    (g, reg, xform_id)
}

#[test]
fn box_through_transform_produces_translated_mesh() {
    let (mut g, reg, xform_id) = box_transform_graph();
    evaluate_all(&mut g, &reg).unwrap();

    let out = g.get(xform_id).unwrap()
        .cached_outputs.get("out").cloned().unwrap();
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
    // Box(2,2,2) is centered at origin, so y values range from -1 to +1.
    // After translate_y=5, y values should range from 4 to 6.
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
    let r = g.allocate_id();
    let e = g.allocate_id();
    let mut rn = NodeInstance::new(r, "Rectangle", [0.0, 0.0]);
    rn.properties.insert("width", PortValue::Number(4.0));
    rn.properties.insert("height", PortValue::Number(2.0));
    let mut en = NodeInstance::new(e, "Extrude", [200.0, 0.0]);
    en.properties.insert("height", PortValue::Number(3.0));
    g.add_node(rn).unwrap();
    g.add_node(en).unwrap();
    g.connect(
        Edge { from: SocketId { node: r, name: "out" }, to: SocketId { node: e, name: "input" } },
        &reg,
    ).unwrap();

    atomartist_lib::graph::executor::evaluate_all(&mut g, &reg).unwrap();
    match g.get(e).unwrap().cached_outputs.get("out") {
        Some(PortValue::Geometry3d(m)) => {
            // Extrude produces caps + sides; vert count > 0, tri count > 0.
            assert!(m.vert_properties.len() > 0);
            assert!(m.tri_verts.len() >= 12);
            // Z extents are ±1.5.
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
        _ => panic!("expected Geometry3d on Extrude.out"),
    }
}

#[test]
fn combine_two_boxes_via_executor() {
    let mut reg = NodeRegistry::new();
    nodes::register_all(&mut reg);
    let mut g = Graph::new();

    let a = g.allocate_id();
    let b = g.allocate_id();
    let c = g.allocate_id();
    g.add_node(NodeInstance::new(a, "Box", [0.0, 0.0])).unwrap();
    g.add_node(NodeInstance::new(b, "Box", [0.0, 100.0])).unwrap();
    g.add_node(NodeInstance::new(c, "Combine", [200.0, 0.0])).unwrap();

    g.connect(
        Edge {
            from: SocketId { node: a, name: "out" },
            to: SocketId { node: c, name: "input_1" },
        },
        &reg,
    ).unwrap();
    g.connect(
        Edge {
            from: SocketId { node: b, name: "out" },
            to: SocketId { node: c, name: "input_2" },
        },
        &reg,
    ).unwrap();

    evaluate_all(&mut g, &reg).unwrap();
    match g.get(c).unwrap().cached_outputs.get("out") {
        Some(PortValue::Geometry3d(m)) => {
            // Each Box has 24 verts, 12 tris; combined → 48, 24.
            assert_eq!(m.vert_properties.len() / m.num_prop as usize, 48);
            assert_eq!(m.tri_verts.len() / 3, 24);
        }
        other => panic!("unexpected output: {:?}", other.map(|v| v.socket_type())),
    }

    // Just make sure we hold the result via Arc — sanity check on the
    // PortValue<->Arc invariant.
    let _arc: Arc<MeshGL> = match g.get(c).unwrap().cached_outputs.get("out") {
        Some(PortValue::Geometry3d(m)) => m.clone(),
        _ => panic!(),
    };
}
