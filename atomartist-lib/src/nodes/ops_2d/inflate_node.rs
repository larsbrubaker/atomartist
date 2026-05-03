//! Inflate — offset a `CrossSection` outward (or inward when delta < 0).
//!
//! Backed by `clipper2-rust` polygon offset; positive delta grows the
//! shape, negative shrinks it. Round joins by default (Clipper2 join_type 0).

use std::sync::Arc;

use crate::graph::node::PortValue;
use crate::geometry::path2d::CrossSection;
use crate::registry::{
    NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry, PropDef, SocketDef,
};
use crate::socket_types::SocketType;

pub struct InflateNode;

impl NodeDef for InflateNode {
    fn type_id(&self) -> &'static str { "Inflate" }
    fn display_name(&self) -> &'static str { "Inflate" }
    fn category(&self) -> &'static str { "Operations 2D" }

    fn input_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("input", SocketType::Path2d)]
    }
    fn output_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("out", SocketType::Path2d)]
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("delta", PortValue::Number(1.0)).with_range(-1000.0, 1000.0),
        ]
    }

    fn evaluate(&self, inputs: &NodeInputs, props: &NodeProperties) -> Result<NodeOutputs, NodeError> {
        let input = match inputs.get("input") {
            PortValue::Path2d(p) => p.clone(),
            PortValue::None => return Ok(NodeOutputs::default()),
            other => return Err(NodeError::msg(format!(
                "Inflate: expected Path2d input, got {:?}", other.socket_type()
            ))),
        };
        let delta = props.number("delta", 1.0);
        let result: CrossSection = input.offset(delta);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Path2d(Arc::new(result)));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(InflateNode);
}
