//! Pyramid primitive node — square base, apex centered above.

use std::sync::Arc;

use crate::geometry::generate_pyramid;
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    wrap_mesh, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, ParamSet,
    PropDef,
};
use crate::socket_types::SocketType;

pub struct PyramidNode;

/// The Pyramid node's parameter schema. Shared `Color` / `Matrix` (via
/// [`ParamSet::geometry`]) lead; the width/height/depth dimensions follow
/// on capitalized sockets, matching the "socket-or-property" shape.
fn params() -> ParamSet {
    ParamSet::geometry()
        .number("width", "Width", 20.0, 0.001..=10_000.0)
        .socket_named("Width")
        .number("height", "Height", 20.0, 0.001..=10_000.0)
        .socket_named("Height")
        .number("depth", "Depth", 20.0, 0.001..=10_000.0)
        .socket_named("Depth")
}

impl NodeDef for PyramidNode {
    fn type_id(&self) -> &'static str { "Pyramid" }
    fn display_name(&self) -> &'static str { "Pyramid" }
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
        let w = rd.number("width");
        let h = rd.number("height");
        let d = rd.number("depth");
        let mut o = NodeOutputs::default();
        o.set("out", PortValue::Geometry3d(Arc::new(wrap_mesh(ctx, generate_pyramid(w, h, d)))));
        Ok(o)
    }
}

pub fn register(reg: &mut NodeRegistry) { reg.register(PyramidNode); }
