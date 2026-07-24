//! Torus primitive node.

use std::sync::Arc;

use crate::geometry::generate_torus;
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    wrap_mesh, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, ParamSet,
    PropDef,
};
use crate::socket_types::SocketType;

pub struct TorusNode;

/// The Torus node's parameter schema. Shared `Color` / `Matrix` (via
/// [`ParamSet::geometry`]) lead; the major/minor radii and segment counts
/// follow on capitalized sockets, matching the "socket-or-property" shape.
fn params() -> ParamSet {
    ParamSet::geometry()
        .number("major_radius", "Major Radius", 10.0, 0.001..=10_000.0)
        .socket_named("Major Radius")
        .number("minor_radius", "Minor Radius", 3.0, 0.001..=10_000.0)
        .socket_named("Minor Radius")
        .number("segments_major", "Segments Major", 32.0, 3.0..=256.0)
        .socket_named("Segments Major")
        .number("segments_minor", "Segments Minor", 16.0, 3.0..=256.0)
        .socket_named("Segments Minor")
}

impl NodeDef for TorusNode {
    fn type_id(&self) -> &'static str { "Torus" }
    fn display_name(&self) -> &'static str { "Torus" }
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
        let major = rd.number("major_radius");
        let minor = rd.number("minor_radius");
        let su = rd.number("segments_major").round().clamp(3.0, 256.0) as u32;
        let sv = rd.number("segments_minor").round().clamp(3.0, 256.0) as u32;
        let mut o = NodeOutputs::default();
        o.set(
            "out",
            PortValue::Geometry3d(Arc::new(wrap_mesh(ctx, generate_torus(major, minor, su, sv)))),
        );
        Ok(o)
    }
}

pub fn register(reg: &mut NodeRegistry) { reg.register(TorusNode); }
