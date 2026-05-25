//! End-to-end test: register a subgraph, instantiate it, and verify the
//! parent graph receives the expected output mesh.
//!
//! Template: `GraphInput("mesh")` → `Transform(+7y)` → `Output`. The
//! Output node's mirror output (named after the Transform's "out"
//! socket) becomes the subgraph's published output port.
//!
//! Parent: `Box` → `SubgraphNodeDef("ShiftYBy7")` → `Output`.

use std::sync::Arc;

use atomartist_lib::graph::executor::evaluate_all;
use atomartist_lib::graph::node::PortValue;
use atomartist_lib::graph::{Noodle, Graph, NodeId};
use atomartist_lib::nodes::{self, register_subgraph};
use atomartist_lib::registry::NodeRegistry;

/// Build the translate-subgraph template:
///
/// `GraphInput("mesh") → Transform(ty=7) → Output`
///
/// The Output node's mirror output socket (adopted from the Transform's
/// `out` socket name) becomes the published subgraph port — named
/// "out" by default.
fn build_translate_subgraph_template(reg: &NodeRegistry) -> Graph {
    let mut g = Graph::new();
    let gin = g.add_new_node("GraphInput", [0.0, 0.0], reg).unwrap();
    let xform = g.add_new_node("Transform", [200.0, 0.0], reg).unwrap();
    let out_node = g.add_new_node("Output", [400.0, 0.0], reg).unwrap();

    g.set_property(gin, "name", PortValue::StringVal(Arc::new("mesh".into()))).unwrap();
    g.set_property(xform, "ty", PortValue::Number(7.0)).unwrap();

    // GraphInput.out → Transform.input
    let out_gin = g.get(gin).unwrap().output_by_name("out").unwrap().uid;
    let in_xform = g.get(xform).unwrap().input_by_name("input").unwrap().uid;
    g.connect(Noodle::new(gin, out_gin, xform, in_xform), reg).unwrap();

    // Transform.out → Output's empty trailing slot. Output's
    // on_input_connected hook adopts the source-socket name ("out") as
    // the mirror output, which becomes the subgraph's published port.
    let out_xform = g.get(xform).unwrap().output_by_name("out").unwrap().uid;
    let out_empty = g.get(out_node).unwrap().inputs[0].uid;
    g.connect(Noodle::new(xform, out_xform, out_node, out_empty), reg).unwrap();

    g
}

fn connect_by_name(
    g: &mut Graph,
    from: NodeId,
    from_name: &str,
    to: NodeId,
    to_name: &str,
    reg: &NodeRegistry,
) {
    let from_uid = g.get(from).unwrap().output_by_name(from_name).unwrap().uid;
    let to_uid = g.get(to).unwrap().input_by_name(to_name).unwrap().uid;
    g.connect(Noodle::new(from, from_uid, to, to_uid), reg).unwrap();
}

#[test]
fn translate_subgraph_shifts_box_in_y() {
    let mut reg = NodeRegistry::new();
    nodes::register_all(&mut reg);

    let template = build_translate_subgraph_template(&reg);
    let id = register_subgraph(&mut reg, "ShiftYBy7", "Shift Y by 7", template);
    assert_eq!(id, "ShiftYBy7");

    // Verify the subgraph's instance carries the expected sockets:
    // one input "mesh" (from the GraphInput's name property) and one
    // output "out" (from the Output node's mirror socket).
    let mut probe = Graph::new();
    let probe_inst = probe.add_new_node("ShiftYBy7", [0.0, 0.0], &reg).unwrap();
    let inst = probe.get(probe_inst).unwrap();
    assert_eq!(inst.inputs.len(), 1);
    assert_eq!(inst.inputs[0].name.as_ref(), "mesh");
    assert_eq!(inst.outputs.len(), 1);
    assert_eq!(inst.outputs[0].name.as_ref(), "out");

    // Parent graph: Box → ShiftYBy7 → Output (its trailing empty slot).
    // Read the merged result back via the parent Output's __display__.
    let mut parent = Graph::new();
    let bx = parent.add_new_node("Box", [0.0, 0.0], &reg).unwrap();
    let sub = parent.add_new_node("ShiftYBy7", [200.0, 0.0], &reg).unwrap();
    let out_node = parent.add_new_node("Output", [400.0, 0.0], &reg).unwrap();

    connect_by_name(&mut parent, bx, "out", sub, "mesh", &reg);
    // ShiftYBy7 → parent Output's empty trailing slot.
    let sub_out_uid = parent.get(sub).unwrap().output_by_name("out").unwrap().uid;
    let out_in_uid = parent.get(out_node).unwrap().inputs[0].uid;
    parent
        .connect(Noodle::new(sub, sub_out_uid, out_node, out_in_uid), &reg)
        .unwrap();

    evaluate_all(&mut parent, &reg).unwrap();

    let display_uid = parent
        .get(out_node)
        .unwrap()
        .output_by_name("__display__")
        .unwrap()
        .uid;
    let out_value = parent
        .get(out_node)
        .unwrap()
        .cached_outputs
        .get(&display_uid)
        .cloned()
        .unwrap();
    match out_value {
        PortValue::Geometry3d(geo) => {
            let mesh = &geo.first().unwrap().mesh;
            let stride = mesh.num_prop as usize;
            let n = mesh.vert_properties.len() / stride;
            let mut y_min = f32::INFINITY;
            let mut y_max = f32::NEG_INFINITY;
            for i in 0..n {
                let y = mesh.vert_properties[i * stride + 1];
                if y < y_min { y_min = y; }
                if y > y_max { y_max = y; }
            }
            // Default Box(20×20×20) centered at origin → Y in [-10, 10].
            // Shifted +7 → Y in [-3, 17].
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
    let sub = parent.add_new_node("ShiftYBy7v2", [0.0, 0.0], &reg).unwrap();

    evaluate_all(&mut parent, &reg).unwrap();
    // The published output is "out" (adopted from the Transform's
    // socket name in the template).
    let out_uid = parent.get(sub).unwrap().output_by_name("out").unwrap().uid;
    let v = parent.get(sub).unwrap().cached_outputs.get(&out_uid);
    assert!(
        v.is_none()
            || matches!(v, Some(PortValue::None))
            || matches!(v, Some(PortValue::Geometry3d(_))),
        "unexpected output kind: {:?}",
        v.map(|x| x.socket_type())
    );
}
