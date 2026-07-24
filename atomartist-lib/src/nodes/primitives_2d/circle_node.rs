//! 2D Circle node — N-segment polygon approximation of a circle.

use std::sync::Arc;

use manifold_rust::cross_section::CrossSection;

use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, ParamSet, PropDef,
};
use crate::socket_types::SocketType;

pub struct CircleNode;

/// The Circle node's parameter schema — the single source from which its
/// sockets, property rows, and `evaluate` reads all derive.
fn params() -> ParamSet {
    ParamSet::new()
        .number("radius", "Radius", 10.0, 0.001..=10_000.0)
        .number("segments", "Segments", 32.0, 3.0..=256.0)
}

impl NodeDef for CircleNode {
    fn type_id(&self) -> &'static str { "Circle" }
    fn display_name(&self) -> &'static str { "Circle" }
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
        let r = ps.reader(ctx);
        let radius = r.number("radius");
        let segs = r.number("segments").round().clamp(3.0, 256.0) as i32;
        let cs = CrossSection::circle(radius, segs);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Path2d(Arc::new(cs)));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(CircleNode);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{executor::evaluate_all, Graph, Noodle};
    use crate::nodes::register_all;
    use crate::registry::NodeRegistry;

    /// Build a Circle graph (radius property = `radius_prop`), optionally
    /// wiring a NumberConst emitting `wired_radius` into the `radius`
    /// socket, evaluate, and return the output bounds width (≈ 2·radius).
    fn eval_circle_width(radius_prop: f64, wired_radius: Option<f64>) -> f64 {
        let mut reg = NodeRegistry::new();
        register_all(&mut reg);
        let mut g = Graph::new();
        let c = g.add_new_node("Circle", [0.0, 0.0], &reg).unwrap();
        g.set_property(c, "radius", PortValue::Number(radius_prop)).unwrap();
        if let Some(r) = wired_radius {
            let nc = g.add_new_node("NumberConst", [-200.0, 0.0], &reg).unwrap();
            g.set_property(nc, "value", PortValue::Number(r)).unwrap();
            let nc_out = g.get(nc).unwrap().output_by_name("out").unwrap().uid;
            let circ_in = g.get(c).unwrap().input_by_name("radius").unwrap().uid;
            g.connect(Noodle::new(nc, nc_out, c, circ_in), &reg).unwrap();
        }
        evaluate_all(&mut g, &reg).unwrap();
        let out_uid = g.get(c).unwrap().output_by_name("out").unwrap().uid;
        match g.get(c).unwrap().cached_outputs.get(&out_uid) {
            Some(PortValue::Path2d(cs)) => cs.bounds().size().x,
            other => panic!("expected Path2d output, got {other:?}"),
        }
    }

    #[test]
    fn radius_socket_overrides_property() {
        // A vertex sits at angle 0 → bounds width is exactly 2·radius.
        let w = eval_circle_width(3.0, Some(7.0));
        assert!((w - 14.0).abs() < 1e-6, "width was {w}");
    }

    #[test]
    fn radius_falls_back_to_property_when_unwired() {
        let w = eval_circle_width(3.0, None);
        assert!((w - 6.0).abs() < 1e-6, "width was {w}");
    }
}
