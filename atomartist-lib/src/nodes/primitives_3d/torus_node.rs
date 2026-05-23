//! Torus primitive node.

use std::sync::Arc;

use crate::geometry::generate_torus;
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, PropDef,
};
use crate::socket_types::SocketType;

pub struct TorusNode;

impl NodeDef for TorusNode {
    fn type_id(&self) -> &'static str { "Torus" }
    fn display_name(&self) -> &'static str { "Torus" }
    fn category(&self) -> &'static str { "Primitives 3D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .output("out", SocketType::Geometry3d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("major_radius", PortValue::Number(10.0)).with_range(0.001, 10_000.0),
            PropDef::new("minor_radius", PortValue::Number(3.0)).with_range(0.001, 10_000.0),
            PropDef::new("segments_major", PortValue::Number(32.0)).with_range(3.0, 256.0),
            PropDef::new("segments_minor", PortValue::Number(16.0)).with_range(3.0, 256.0),
        ]
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let major = ctx.properties.number("major_radius", 10.0);
        let minor = ctx.properties.number("minor_radius", 3.0);
        let su = ctx.properties.number("segments_major", 32.0).round().clamp(3.0, 256.0) as u32;
        let sv = ctx.properties.number("segments_minor", 16.0).round().clamp(3.0, 256.0) as u32;
        let mut o = NodeOutputs::default();
        o.set("out", PortValue::Geometry3d(Arc::new(generate_torus(major, minor, su, sv))));
        Ok(o)
    }
}

pub fn register(reg: &mut NodeRegistry) { reg.register(TorusNode); }
