//! 2D Ring node — `outer_circle.difference(inner_circle)` annulus.

use std::sync::Arc;

use manifold_rust::cross_section::CrossSection;

use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, ParamSet, PropDef,
};
use crate::socket_types::SocketType;

pub struct RingNode;

/// The Ring node's parameter schema — the single source from which its
/// sockets, property rows, and `evaluate` reads all derive.
fn params() -> ParamSet {
    ParamSet::new()
        .number("outer_radius", "Outer Radius", 10.0, 0.001..=10_000.0)
        .number("inner_radius", "Inner Radius", 6.0, 0.001..=10_000.0)
        .number("segments", "Segments", 32.0, 3.0..=256.0)
}

impl NodeDef for RingNode {
    fn type_id(&self) -> &'static str { "Ring" }
    fn display_name(&self) -> &'static str { "Ring" }
    fn category(&self) -> &'static str { "Primitives 2D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        params()
            .mint_sockets(InstanceTemplate::builder(alloc))
            .output("out", SocketType::Path2d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        params().prop_defs()
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let ps = params();
        let rd = ps.reader(ctx);
        let r_out = rd.number("outer_radius");
        let r_in = rd.number("inner_radius").min(r_out - 1e-6).max(0.0);
        let segs = rd.number("segments").round().clamp(3.0, 256.0) as i32;
        let outer = CrossSection::circle(r_out, segs);
        let cs = if r_in > 1e-6 {
            let inner = CrossSection::circle(r_in, segs);
            outer.difference(&inner)
        } else {
            outer
        };
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Path2d(Arc::new(cs)));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(RingNode);
}
