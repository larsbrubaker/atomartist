//! 2D Ring node — `outer_circle.difference(inner_circle)` annulus.

use std::sync::Arc;

use manifold_rust::cross_section::CrossSection;

use crate::graph::node::PortValue;
use crate::registry::{
    NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry, PropDef, SocketDef,
};
use crate::socket_types::SocketType;

pub struct RingNode;

impl NodeDef for RingNode {
    fn type_id(&self) -> &'static str { "Ring" }
    fn display_name(&self) -> &'static str { "Ring" }
    fn category(&self) -> &'static str { "Primitives 2D" }

    fn input_sockets(&self) -> Vec<SocketDef> { vec![] }
    fn output_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("out", SocketType::Path2d)]
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("outer_radius", PortValue::Number(10.0)).with_range(0.001, 10_000.0),
            PropDef::new("inner_radius", PortValue::Number(6.0)).with_range(0.001, 10_000.0),
            PropDef::new("segments", PortValue::Number(32.0)).with_range(3.0, 256.0),
        ]
    }

    fn evaluate(&self, _inputs: &NodeInputs, props: &NodeProperties) -> Result<NodeOutputs, NodeError> {
        let r_out = props.number("outer_radius", 10.0);
        let r_in = props.number("inner_radius", 6.0).min(r_out - 1e-6).max(0.0);
        let segs = props.number("segments", 32.0).round().clamp(3.0, 256.0) as i32;
        let outer = CrossSection::circle(r_out, segs);
        let cs = if r_in > 1e-6 {
            let inner = CrossSection::circle(r_in, segs);
            outer.difference(&inner)
        } else {
            outer
        };
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Path2d(Arc::new(cs)));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(RingNode);
}
