//! Cone primitive node.

use std::sync::Arc;

use crate::geometry::generate_cone;
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    geometry_props, wrap_mesh, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs,
    NodeRegistry, PropDef,
};
use crate::socket_types::SocketType;

pub struct ConeNode;

impl NodeDef for ConeNode {
    fn type_id(&self) -> &'static str { "Cone" }
    fn display_name(&self) -> &'static str { "Cone" }
    fn category(&self) -> &'static str { "Primitives 3D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .output("out", SocketType::Geometry3d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        let mut p = vec![
            PropDef::new("radius", PortValue::Number(10.0)).with_range(0.001, 10_000.0),
            PropDef::new("height", PortValue::Number(20.0)).with_range(0.001, 10_000.0),
            PropDef::new("segments", PortValue::Number(32.0)).with_range(3.0, 256.0),
        ];
        // Prepend color + matrix so they render as the first two rows.
        let mut p = { let mut g = geometry_props(); g.extend(p); g };
        p
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let r = ctx.properties.number("radius", 10.0);
        let h = ctx.properties.number("height", 20.0);
        let s = ctx.properties.number("segments", 32.0).round().clamp(3.0, 256.0) as u32;
        let mut o = NodeOutputs::default();
        o.set("out", PortValue::Geometry3d(Arc::new(wrap_mesh(ctx, generate_cone(r, h, s)))));
        Ok(o)
    }
}

pub fn register(reg: &mut NodeRegistry) { reg.register(ConeNode); }
