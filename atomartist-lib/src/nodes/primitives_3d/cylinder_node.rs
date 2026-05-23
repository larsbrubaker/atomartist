//! Cylinder primitive node — Y-axis cylinder centered at origin.

use std::sync::Arc;

use crate::geometry::generate_cylinder;
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, PropDef,
};
use crate::socket_types::SocketType;

pub struct CylinderNode;

impl NodeDef for CylinderNode {
    fn type_id(&self) -> &'static str { "Cylinder" }
    fn display_name(&self) -> &'static str { "Cylinder" }
    fn category(&self) -> &'static str { "Primitives 3D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .output("out", SocketType::Geometry3d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("radius", PortValue::Number(10.0)).with_range(0.001, 10_000.0),
            PropDef::new("height", PortValue::Number(20.0)).with_range(0.001, 10_000.0),
            PropDef::new("segments", PortValue::Number(32.0)).with_range(3.0, 256.0),
        ]
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let r = ctx.properties.number("radius", 10.0);
        let h = ctx.properties.number("height", 20.0);
        let segments = ctx.properties.number("segments", 32.0).round().clamp(3.0, 256.0) as u32;
        let mesh = generate_cylinder(r, h, segments);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Geometry3d(Arc::new(mesh)));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(CylinderNode);
}
