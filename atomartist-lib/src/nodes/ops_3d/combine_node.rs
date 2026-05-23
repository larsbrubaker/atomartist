//! Combine node — merges multiple geometries into one mesh.
//!
//! Eight optional `Geometry3d` inputs (`input_1` .. `input_8`). Inputs that
//! are unconnected or carry empty meshes are skipped. The result is the
//! straight concatenation; for proper Boolean union with surface healing,
//! use the dedicated Boolean node.
//!
//! Stage 1i in the engine refactor plan will rebuild this on the new
//! dynamic-input mechanism (same as Output). For now it preserves the
//! 8-fixed-slot behavior so the port-everything-to-instantiate stage
//! stays mechanical.

use std::sync::Arc;

use crate::geometry::{merge_meshes, num_tris, num_verts};
use crate::graph::node::{NodeId, NodeInstance, PortValue};
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties,
    NodeRegistry,
};
use crate::socket_types::SocketType;

pub struct CombineNode;

const INPUT_NAMES: [&str; 8] = [
    "input_1", "input_2", "input_3", "input_4",
    "input_5", "input_6", "input_7", "input_8",
];

impl NodeDef for CombineNode {
    fn type_id(&self) -> &'static str { "Combine" }
    fn display_name(&self) -> &'static str { "Combine" }
    fn category(&self) -> &'static str { "Operations 3D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        let mut b = InstanceTemplate::builder(alloc);
        for n in &INPUT_NAMES {
            b = b.input_opt(*n, SocketType::Geometry3d);
        }
        b.output("out", SocketType::Geometry3d).build()
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let mut parts = Vec::new();
        for name in &INPUT_NAMES {
            if let PortValue::Geometry3d(m) = ctx.input_named(name) {
                if num_verts(m) > 0 && num_tris(m) > 0 {
                    parts.push(m.clone());
                }
            }
        }
        let merged = merge_meshes(&parts);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Geometry3d(Arc::new(merged)));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(CombineNode);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::generate_box;

    /// Build a ready-to-evaluate (instance, NodeInputs) pair seeded with
    /// the given by-name inputs. Mirrors what the executor does.
    fn setup(
        node: &impl NodeDef,
        named_inputs: &[(&str, PortValue)],
    ) -> (NodeInstance, NodeInputs) {
        let mut alloc = SocketUidAlloc::new();
        let tpl = node.instantiate(&mut alloc);
        let mut inst = NodeInstance::new(NodeId(1), node.type_id().to_string(), [0.0, 0.0]);
        inst.inputs = tpl.inputs;
        inst.outputs = tpl.outputs;
        let mut inputs = NodeInputs::default();
        for (name, value) in named_inputs {
            let uid = inst.input_by_name(name).unwrap().uid;
            inputs.insert(uid, value.clone());
        }
        (inst, inputs)
    }

    #[test]
    fn combine_two_boxes() {
        let n = CombineNode;
        let (inst, inputs) = setup(
            &n,
            &[
                ("input_1", PortValue::Geometry3d(Arc::new(generate_box(1.0, 1.0, 1.0)))),
                ("input_2", PortValue::Geometry3d(Arc::new(generate_box(1.0, 1.0, 1.0)))),
            ],
        );
        let props = NodeProperties::default();
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
        let outs = n.evaluate(&ctx).unwrap();
        match outs.by_name.get("out").unwrap() {
            PortValue::Geometry3d(m) => {
                assert_eq!(num_verts(m), 48);
                assert_eq!(num_tris(m), 24);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn combine_skips_empty_inputs() {
        let n = CombineNode;
        let (inst, inputs) = setup(
            &n,
            &[
                ("input_1", PortValue::Geometry3d(Arc::new(generate_box(1.0, 1.0, 1.0)))),
            ],
        );
        let props = NodeProperties::default();
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
        let outs = n.evaluate(&ctx).unwrap();
        match outs.by_name.get("out").unwrap() {
            PortValue::Geometry3d(m) => {
                assert_eq!(num_verts(m), 24);
                assert_eq!(num_tris(m), 12);
            }
            _ => panic!(),
        }
    }
}
