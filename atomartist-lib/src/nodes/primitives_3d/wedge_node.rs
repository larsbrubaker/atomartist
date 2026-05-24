//! Wedge primitive node — triangular prism (right-triangle cross-section).

use std::sync::Arc;

use crate::geometry::generate_wedge;
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    geometry_props, wrap_mesh, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs,
    NodeRegistry, PropDef,
};
use crate::socket_types::SocketType;

pub struct WedgeNode;

impl NodeDef for WedgeNode {
    fn type_id(&self) -> &'static str { "Wedge" }
    fn display_name(&self) -> &'static str { "Wedge" }
    fn category(&self) -> &'static str { "Primitives 3D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .output("out", SocketType::Geometry3d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        let mut p = vec![
            PropDef::new("width", PortValue::Number(20.0)).with_range(0.001, 10_000.0),
            PropDef::new("height", PortValue::Number(20.0)).with_range(0.001, 10_000.0),
            PropDef::new("depth", PortValue::Number(20.0)).with_range(0.001, 10_000.0),
        ];
        // Prepend color + matrix so they render as the first two rows.
        let mut p = { let mut g = geometry_props(); g.extend(p); g };
        p
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let w = ctx.properties.number("width", 20.0);
        let h = ctx.properties.number("height", 20.0);
        let d = ctx.properties.number("depth", 20.0);
        let mut o = NodeOutputs::default();
        o.set("out", PortValue::Geometry3d(Arc::new(wrap_mesh(ctx, generate_wedge(w, h, d)))));
        Ok(o)
    }
}

pub fn register(reg: &mut NodeRegistry) { reg.register(WedgeNode); }
