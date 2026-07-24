//! Inflate — offset a `CrossSection` outward (or inward when delta < 0).
//!
//! Backed by `clipper2-rust` polygon offset; positive delta grows the
//! shape, negative shrinks it. Round joins by default (Clipper2 join_type 0).

use std::sync::Arc;

use crate::geometry::path2d::CrossSection;
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, ParamSet, PropDef,
};
use crate::socket_types::SocketType;

pub struct InflateNode;

/// The Inflate node's `delta` parameter — the single source from which
/// its optional socket, property row, and `evaluate` read all derive.
/// The required `input` Path2d socket is minted separately (it leads).
fn params() -> ParamSet {
    ParamSet::new().number("delta", "Delta", 1.0, -1000.0..=1000.0)
}

impl NodeDef for InflateNode {
    fn type_id(&self) -> &'static str { "Inflate" }
    fn display_name(&self) -> &'static str { "Inflate" }
    fn category(&self) -> &'static str { "Operations 2D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        params()
            .mint_sockets(
                InstanceTemplate::builder(alloc).input("input", SocketType::Path2d),
            )
            .output("out", SocketType::Path2d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        params().prop_defs()
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let input = match ctx.input_named("input") {
            PortValue::Path2d(p) => p.clone(),
            PortValue::None => return Ok(NodeOutputs::default()),
            other => return Err(NodeError::msg(format!(
                "Inflate: expected Path2d input, got {:?}", other.socket_type()
            ))),
        };
        let delta = params().reader(ctx).number("delta");
        let result: CrossSection = input.offset(delta);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Path2d(Arc::new(result)));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(InflateNode);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{executor::evaluate_all, Graph, Noodle};
    use crate::nodes::register_all;
    use crate::registry::NodeRegistry;

    /// Feed a 4×4 Rectangle into Inflate (delta property = `delta_prop`),
    /// optionally wiring a NumberConst emitting `wired_delta` into the
    /// `delta` socket, evaluate, and return the inflated bounds width.
    /// For a rectangle with round joins the max x-extent is
    /// `width + 2·delta`.
    fn eval_inflate_width(delta_prop: f64, wired_delta: Option<f64>) -> f64 {
        let mut reg = NodeRegistry::new();
        register_all(&mut reg);
        let mut g = Graph::new();
        let rect = g.add_new_node("Rectangle", [0.0, 0.0], &reg).unwrap();
        g.set_property(rect, "width", PortValue::Number(4.0)).unwrap();
        g.set_property(rect, "height", PortValue::Number(4.0)).unwrap();
        let inf = g.add_new_node("Inflate", [200.0, 0.0], &reg).unwrap();
        g.set_property(inf, "delta", PortValue::Number(delta_prop)).unwrap();
        let rout = g.get(rect).unwrap().output_by_name("out").unwrap().uid;
        let iin = g.get(inf).unwrap().input_by_name("input").unwrap().uid;
        g.connect(Noodle::new(rect, rout, inf, iin), &reg).unwrap();
        if let Some(d) = wired_delta {
            let nc = g.add_new_node("NumberConst", [-200.0, 0.0], &reg).unwrap();
            g.set_property(nc, "value", PortValue::Number(d)).unwrap();
            let nc_out = g.get(nc).unwrap().output_by_name("out").unwrap().uid;
            let inf_delta = g.get(inf).unwrap().input_by_name("delta").unwrap().uid;
            g.connect(Noodle::new(nc, nc_out, inf, inf_delta), &reg).unwrap();
        }
        evaluate_all(&mut g, &reg).unwrap();
        let out_uid = g.get(inf).unwrap().output_by_name("out").unwrap().uid;
        match g.get(inf).unwrap().cached_outputs.get(&out_uid) {
            Some(PortValue::Path2d(cs)) => cs.bounds().size().x,
            other => panic!("expected Path2d output, got {other:?}"),
        }
    }

    #[test]
    fn delta_socket_overrides_property() {
        // Property says 0.5 but the wired NumberConst says 2.0 →
        // width 4 + 2·2 = 8.
        let w = eval_inflate_width(0.5, Some(2.0));
        assert!((w - 8.0).abs() < 1e-4, "width was {w}");
    }

    #[test]
    fn delta_falls_back_to_property_when_unwired() {
        // 4 + 2·0.5 = 5.
        let w = eval_inflate_width(0.5, None);
        assert!((w - 5.0).abs() < 1e-4, "width was {w}");
    }
}
