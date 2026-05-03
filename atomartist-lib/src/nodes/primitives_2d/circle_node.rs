//! 2D Circle node — N-segment polygon approximation of a circle.

use std::sync::Arc;

use manifold_rust::cross_section::CrossSection;

use crate::graph::node::PortValue;
use crate::registry::{
    NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry, PropDef, SocketDef,
};
use crate::socket_types::SocketType;

pub struct CircleNode;

impl NodeDef for CircleNode {
    fn type_id(&self) -> &'static str { "Circle" }
    fn display_name(&self) -> &'static str { "Circle" }
    fn category(&self) -> &'static str { "Primitives 2D" }

    fn input_sockets(&self) -> Vec<SocketDef> { vec![] }
    fn output_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("out", SocketType::Path2d)]
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("radius", PortValue::Number(10.0)).with_range(0.001, 10_000.0),
            PropDef::new("segments", PortValue::Number(32.0)).with_range(3.0, 256.0),
        ]
    }

    fn evaluate(&self, _inputs: &NodeInputs, props: &NodeProperties) -> Result<NodeOutputs, NodeError> {
        let r = props.number("radius", 10.0);
        let segs = props.number("segments", 32.0).round().clamp(3.0, 256.0) as i32;
        let cs = CrossSection::circle(r, segs);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Path2d(Arc::new(cs)));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(CircleNode);
}
