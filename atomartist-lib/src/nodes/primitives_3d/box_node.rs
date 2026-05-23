//! Box primitive node — generates an axis-aligned cuboid centered at origin.

use std::sync::Arc;

use crate::geometry::generate_box;
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, PropDef,
};
use crate::socket_types::SocketType;

pub struct BoxNode;

impl NodeDef for BoxNode {
    fn type_id(&self) -> &'static str { "Box" }
    fn display_name(&self) -> &'static str { "Box" }
    fn category(&self) -> &'static str { "Primitives 3D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .output("out", SocketType::Geometry3d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("width", PortValue::Number(20.0)).with_range(0.001, 10_000.0),
            PropDef::new("height", PortValue::Number(20.0)).with_range(0.001, 10_000.0),
            PropDef::new("depth", PortValue::Number(20.0)).with_range(0.001, 10_000.0),
        ]
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let w = ctx.properties.number("width", 20.0);
        let h = ctx.properties.number("height", 20.0);
        let d = ctx.properties.number("depth", 20.0);
        let mesh = generate_box(w, h, d);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Geometry3d(Arc::new(mesh)));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(BoxNode);
}
