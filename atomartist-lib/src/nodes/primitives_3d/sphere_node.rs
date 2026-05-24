//! Sphere primitive node — UV sphere centered at origin.

use std::sync::Arc;

use crate::geometry::generate_sphere;
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    geometry_props, wrap_mesh, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs,
    NodeRegistry, PropDef,
};
use crate::socket_types::SocketType;

pub struct SphereNode;

impl NodeDef for SphereNode {
    fn type_id(&self) -> &'static str { "Sphere" }
    fn display_name(&self) -> &'static str { "Sphere" }
    fn category(&self) -> &'static str { "Primitives 3D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .output("out", SocketType::Geometry3d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        let mut p = vec![
            PropDef::new("radius", PortValue::Number(10.0)).with_range(0.001, 10_000.0),
            PropDef::new("segments_u", PortValue::Number(32.0)).with_range(3.0, 256.0),
            PropDef::new("segments_v", PortValue::Number(16.0)).with_range(2.0, 256.0),
        ];
        // Prepend color + matrix so they render as the first two rows.
        let mut p = { let mut g = geometry_props(); g.extend(p); g };
        p
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let r = ctx.properties.number("radius", 10.0);
        let su = ctx.properties.number("segments_u", 32.0).round().clamp(3.0, 256.0) as u32;
        let sv = ctx.properties.number("segments_v", 16.0).round().clamp(2.0, 256.0) as u32;
        let mesh = generate_sphere(r, su, sv);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Geometry3d(Arc::new(wrap_mesh(ctx, mesh))));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(SphereNode);
}
