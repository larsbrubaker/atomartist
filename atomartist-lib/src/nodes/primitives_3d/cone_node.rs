//! Cone primitive node.

use std::sync::Arc;

use crate::geometry::generate_cone;
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    wrap_mesh, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, ParamSet,
    PropDef,
};
use crate::socket_types::SocketType;

pub struct ConeNode;

/// The Cone node's parameter schema. Shared `Color` / `Matrix` (via
/// [`ParamSet::geometry`]) lead; the scalar dimensions follow on
/// capitalized sockets, matching the "socket-or-property" shape.
fn params() -> ParamSet {
    ParamSet::primitive("Cone", 10.0)
        .number("radius", "Radius", 10.0, 0.001..=10_000.0)
        .socket_named("Radius")
        .number("height", "Height", 20.0, 0.001..=10_000.0)
        .socket_named("Height")
        .number("segments", "Segments", 32.0, 3.0..=256.0)
        .socket_named("Segments")
}

impl NodeDef for ConeNode {
    fn type_id(&self) -> &'static str { "Cone" }
    fn display_name(&self) -> &'static str { "Cone" }
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
        let h = rd.number("height");
        let s = rd.number("segments").round().clamp(3.0, 256.0) as u32;
        let mut o = NodeOutputs::default();
        o.set("out", PortValue::Geometry3d(Arc::new(wrap_mesh(ctx, generate_cone(r, h, s)))));
        Ok(o)
    }
}

pub fn register(reg: &mut NodeRegistry) { reg.register(ConeNode); }
