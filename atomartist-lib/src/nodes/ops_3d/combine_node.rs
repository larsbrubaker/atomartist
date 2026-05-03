//! Combine node — merges multiple geometries into one mesh.
//!
//! Eight optional `Geometry3d` inputs (`input_1` .. `input_8`). Inputs that
//! are unconnected or carry empty meshes are skipped. The result is the
//! straight concatenation; for proper Boolean union with surface healing,
//! use the dedicated Boolean node (Phase 8).

use std::sync::Arc;

use crate::geometry::{merge_meshes, num_tris, num_verts};
use crate::graph::node::PortValue;
use crate::registry::{
    NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry, SocketDef,
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

    fn input_sockets(&self) -> Vec<SocketDef> {
        INPUT_NAMES.iter().map(|n| SocketDef::optional(n, SocketType::Geometry3d)).collect()
    }

    fn output_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("out", SocketType::Geometry3d)]
    }

    fn evaluate(&self, inputs: &NodeInputs, _props: &NodeProperties) -> Result<NodeOutputs, NodeError> {
        let mut parts = Vec::new();
        for name in &INPUT_NAMES {
            if let PortValue::Geometry3d(m) = inputs.get(name) {
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

    #[test]
    fn combine_two_boxes() {
        let n = CombineNode;
        let mut inputs = NodeInputs::default();
        inputs.insert("input_1", PortValue::Geometry3d(Arc::new(generate_box(1.0, 1.0, 1.0))));
        inputs.insert("input_2", PortValue::Geometry3d(Arc::new(generate_box(1.0, 1.0, 1.0))));
        let outs = n.evaluate(&inputs, &NodeProperties::default()).unwrap();
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
        let mut inputs = NodeInputs::default();
        inputs.insert("input_1", PortValue::Geometry3d(Arc::new(generate_box(1.0, 1.0, 1.0))));
        // input_2 is unconnected — defaults to PortValue::None
        let outs = n.evaluate(&inputs, &NodeProperties::default()).unwrap();
        match outs.by_name.get("out").unwrap() {
            PortValue::Geometry3d(m) => {
                assert_eq!(num_verts(m), 24);
                assert_eq!(num_tris(m), 12);
            }
            _ => panic!(),
        }
    }
}
