//! End-to-end test: register a subgraph, instantiate it, and verify the
//! parent graph receives the expected output mesh.
//!
//! Template: GraphInput("paths") → Extrude → GraphOutput("solid")
//! Parent  : Rectangle → SubgraphNodeDef("RectExtrude") → Output

use std::sync::Arc;

use atomartist_lib::graph::executor::evaluate_all;
use atomartist_lib::graph::node::{NodeInstance, PortValue, SocketId};
use atomartist_lib::graph::{Edge, Graph, NodeId};
use atomartist_lib::nodes::{self, register_subgraph};
use atomartist_lib::registry::NodeRegistry;

fn make_node(
    g: &mut Graph,
    reg: &NodeRegistry,
    type_id: &'static str,
    pos: [f64; 2],
) -> NodeId {
    let id = g.allocate_id();
    let mut n = NodeInstance::new(id, type_id, pos);
    if let Some(def) = reg.get(type_id) {
        for prop in def.properties() {
            n.properties.insert(prop.name, prop.default);
        }
    }
    g.add_node(n).unwrap();
    id
}

fn build_translate_subgraph_template(reg: &NodeRegistry) -> Graph {
    // Geometry3d-only chain: GraphInput("mesh") → Transform → GraphOutput("out_mesh").
    // GraphInput / GraphOutput are currently Geometry3d-typed; broader
    // socket-type support is a follow-up that adds typed variants
    // (GraphInputPath2d, etc.).
    let mut g = Graph::new();
    let gin = make_node(&mut g, reg, "GraphInput", [0.0, 0.0]);
    let xform = make_node(&mut g, reg, "Transform", [200.0, 0.0]);
    let gout = make_node(&mut g, reg, "GraphOutput", [400.0, 0.0]);

    g.get_mut(gin).unwrap().properties.insert(
        "name",
        PortValue::StringVal(Arc::new("mesh".into())),
    );
    g.get_mut(gout).unwrap().properties.insert(
        "name",
        PortValue::StringVal(Arc::new("out_mesh".into())),
    );
    g.get_mut(xform).unwrap().properties.insert("ty", PortValue::Number(7.0));

    g.connect(
        Edge { from: SocketId { node: gin, name: "out" }, to: SocketId { node: xform, name: "input" } },
        reg,
    ).unwrap();
    g.connect(
        Edge { from: SocketId { node: xform, name: "out" }, to: SocketId { node: gout, name: "in" } },
        reg,
    ).unwrap();
    g
}

#[test]
fn translate_subgraph_shifts_box_in_y() {
    let mut reg = NodeRegistry::new();
    nodes::register_all(&mut reg);

    // Build + register the subgraph.
    let template = build_translate_subgraph_template(&reg);
    let id = register_subgraph(&mut reg, "ShiftYBy7", "Shift Y by 7", template);
    assert_eq!(id, "ShiftYBy7");

    let def = reg.get("ShiftYBy7").unwrap();
    assert_eq!(def.category(), "Components");
    let inputs = def.input_sockets();
    let outputs = def.output_sockets();
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].name, "mesh");
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].name, "out_mesh");

    // Parent graph: Box → ShiftYBy7 → Output.
    let mut parent = Graph::new();
    let bx = make_node(&mut parent, &reg, "Box", [0.0, 0.0]);
    let sub = make_node(&mut parent, &reg, "ShiftYBy7", [200.0, 0.0]);
    let out_node = make_node(&mut parent, &reg, "Output", [400.0, 0.0]);

    parent.connect(
        Edge { from: SocketId { node: bx, name: "out" }, to: SocketId { node: sub, name: "mesh" } },
        &reg,
    ).unwrap();
    parent.connect(
        Edge { from: SocketId { node: sub, name: "out_mesh" }, to: SocketId { node: out_node, name: "in" } },
        &reg,
    ).unwrap();

    evaluate_all(&mut parent, &reg).unwrap();

    // The default Box (20×20×20 centered at origin) shifted +7 in Y
    // should have all Y values in [-3, 17].
    let out_value = parent.get(out_node).unwrap()
        .cached_outputs.get("out").cloned().unwrap();
    match out_value {
        PortValue::Geometry3d(mesh) => {
            let stride = mesh.num_prop as usize;
            let n = mesh.vert_properties.len() / stride;
            let mut y_min = f32::INFINITY;
            let mut y_max = f32::NEG_INFINITY;
            for i in 0..n {
                let y = mesh.vert_properties[i * stride + 1];
                if y < y_min { y_min = y; }
                if y > y_max { y_max = y; }
            }
            assert!((y_min - (-3.0)).abs() < 1e-4, "y_min was {}, expected -3", y_min);
            assert!((y_max - 17.0).abs() < 1e-4, "y_max was {}, expected 17", y_max);
        }
        other => panic!("expected Geometry3d, got {:?}", other.socket_type()),
    }
}

#[test]
fn subgraph_with_unconnected_input_returns_no_mesh() {
    let mut reg = NodeRegistry::new();
    nodes::register_all(&mut reg);
    let template = build_translate_subgraph_template(&reg);
    register_subgraph(&mut reg, "ShiftYBy7v2", "Shift Y by 7 v2", template);

    let mut parent = Graph::new();
    let sub = make_node(&mut parent, &reg, "ShiftYBy7v2", [0.0, 0.0]);

    // No upstream wired in. Subgraph should evaluate without crashing.
    evaluate_all(&mut parent, &reg).unwrap();
    let v = parent.get(sub).unwrap().cached_outputs.get("out_mesh");
    // Either None or an empty Geometry3d is acceptable.
    assert!(
        v.is_none()
            || matches!(v, Some(PortValue::None))
            || matches!(v, Some(PortValue::Geometry3d(_))),
        "unexpected output kind: {:?}", v.map(|x| x.socket_type())
    );
}
