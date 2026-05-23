//! 2D Circle node — N-segment polygon approximation of a circle.

use std::sync::Arc;

use manifold_rust::cross_section::CrossSection;

use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, PropDef,
};
use crate::socket_types::SocketType;

pub struct CircleNode;

impl NodeDef for CircleNode {
    fn type_id(&self) -> &'static str { "Circle" }
    fn display_name(&self) -> &'static str { "Circle" }
    fn category(&self) -> &'static str { "Primitives 2D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .output("out", SocketType::Path2d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("radius", PortValue::Number(10.0)).with_range(0.001, 10_000.0),
            PropDef::new("segments", PortValue::Number(32.0)).with_range(3.0, 256.0),
        ]
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let r = ctx.properties.number("radius", 10.0);
        let segs = ctx.properties.number("segments", 32.0).round().clamp(3.0, 256.0) as i32;
        let cs = CrossSection::circle(r, segs);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Path2d(Arc::new(cs)));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(CircleNode);
}
