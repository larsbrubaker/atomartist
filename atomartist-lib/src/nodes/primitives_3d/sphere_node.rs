//! Sphere primitive node — UV sphere centered at origin.

use std::sync::Arc;

use crate::geometry::generate_sphere;
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    wrap_mesh, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, ParamSet,
    PropDef,
};
use crate::socket_types::SocketType;

pub struct SphereNode;

/// The Sphere node's parameter schema. Shared `Color` / `Matrix` (via
/// [`ParamSet::geometry`]) lead; the radius + U/V segment counts follow on
/// capitalized sockets, matching the "socket-or-property" shape.
fn params() -> ParamSet {
    ParamSet::geometry()
        .number("radius", "Radius", 10.0, 0.001..=10_000.0)
        .socket_named("Radius")
        .number("segments_u", "Segments U", 32.0, 3.0..=256.0)
        .socket_named("Segments U")
        .number("segments_v", "Segments V", 16.0, 2.0..=256.0)
        .socket_named("Segments V")
}

impl NodeDef for SphereNode {
    fn type_id(&self) -> &'static str { "Sphere" }
    fn display_name(&self) -> &'static str { "Sphere" }
    fn category(&self) -> &'static str { "Primitives 3D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        params()
            .mint_sockets(InstanceTemplate::builder(alloc))
            .output("out", SocketType::Geometry3d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        params().prop_defs()
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let ps = params();
        let rd = ps.reader(ctx);
        let r = rd.number("radius");
        let su = rd.number("segments_u").round().clamp(3.0, 256.0) as u32;
        let sv = rd.number("segments_v").round().clamp(2.0, 256.0) as u32;
        let mesh = generate_sphere(r, su, sv);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Geometry3d(Arc::new(wrap_mesh(ctx, mesh))));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(SphereNode);
}
