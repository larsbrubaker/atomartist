//! SmoothPaths — applies the CrossSection `simplify` pass to remove
//! micro-segments while preserving the overall shape. Useful as a
//! cleanup step after Boolean ops.

use std::sync::Arc;

use crate::graph::node::PortValue;
use crate::registry::{
    NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry, PropDef, SocketDef,
};
use crate::socket_types::SocketType;

pub struct SmoothPathsNode;

impl NodeDef for SmoothPathsNode {
    fn type_id(&self) -> &'static str { "SmoothPaths" }
    fn display_name(&self) -> &'static str { "Smooth Paths" }
    fn category(&self) -> &'static str { "Operations 2D" }

    fn input_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("input", SocketType::Path2d)]
    }
    fn output_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("out", SocketType::Path2d)]
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![PropDef::new("epsilon", PortValue::Number(0.05)).with_range(0.0001, 10.0)]
    }

    fn evaluate(&self, inputs: &NodeInputs, props: &NodeProperties) -> Result<NodeOutputs, NodeError> {
        let input = match inputs.get("input") {
            PortValue::Path2d(p) => p.clone(),
            PortValue::None => return Ok(NodeOutputs::default()),
            other => return Err(NodeError::msg(format!(
                "SmoothPaths: expected Path2d, got {:?}", other.socket_type()
            ))),
        };
        let eps = props.number("epsilon", 0.05).max(0.0);
        let cleaned = input.simplify(eps);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Path2d(Arc::new(cleaned)));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) { reg.register(SmoothPathsNode); }
