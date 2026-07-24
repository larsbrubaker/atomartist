//! 2D Star node — N-pointed star with alternating outer / inner radii.

use std::sync::Arc;

use manifold_rust::cross_section::CrossSection;
use manifold_rust::linalg::Vec2;

use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, ParamSet, PropDef,
};
use crate::socket_types::SocketType;

pub struct StarNode;

/// The Star node's parameter schema — the single source from which its
/// sockets, property rows, and `evaluate` reads all derive.
fn params() -> ParamSet {
    ParamSet::new()
        .number("points", "Points", 5.0, 3.0..=64.0)
        .number("outer_radius", "Outer Radius", 10.0, 0.001..=10_000.0)
        .number("inner_radius", "Inner Radius", 4.0, 0.001..=10_000.0)
}

impl NodeDef for StarNode {
    fn type_id(&self) -> &'static str { "Star" }
    fn display_name(&self) -> &'static str { "Star" }
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
        let n = rd.number("points").round().clamp(3.0, 64.0) as usize;
        let r_out = rd.number("outer_radius");
        let r_in = rd.number("inner_radius").min(r_out);
        let total = n * 2;
        let mut contour = Vec::with_capacity(total);
        for i in 0..total {
            let angle = (i as f64) * std::f64::consts::TAU / (total as f64) - std::f64::consts::FRAC_PI_2;
            let r = if i % 2 == 0 { r_out } else { r_in };
            contour.push(Vec2::new(r * angle.cos(), r * angle.sin()));
        }
        let cs = CrossSection::from_polygons_fill(vec![contour]);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Path2d(Arc::new(cs)));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(StarNode);
}
