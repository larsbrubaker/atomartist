//! 2D Rectangle node — outputs a `CrossSection` quad in XY.

use std::sync::Arc;

use manifold_rust::cross_section::CrossSection;
use manifold_rust::linalg::Vec2;

use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, ParamSet, PropDef,
};
use crate::socket_types::SocketType;

pub struct RectangleNode;

/// The Rectangle node's parameter schema — the single source from which
/// its sockets, property rows, and `evaluate` reads all derive.
fn params() -> ParamSet {
    ParamSet::new()
        .number("width", "Width", 20.0, 0.001..=10_000.0)
        .number("height", "Height", 20.0, 0.001..=10_000.0)
}

impl NodeDef for RectangleNode {
    fn type_id(&self) -> &'static str { "Rectangle" }
    fn display_name(&self) -> &'static str { "Rectangle" }
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
        let w = r.number("width");
        let h = r.number("height");
        let half_w = w * 0.5;
        let half_h = h * 0.5;
        let contour = vec![
            Vec2::new(-half_w, -half_h),
            Vec2::new( half_w, -half_h),
            Vec2::new( half_w,  half_h),
            Vec2::new(-half_w,  half_h),
        ];
        let cs = CrossSection::from_polygons_fill(vec![contour]);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Path2d(Arc::new(cs)));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(RectangleNode);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::{NodeId, NodeInstance};
    use crate::graph::{executor::evaluate_all, Graph, Noodle};
    use crate::nodes::register_all;
    use crate::registry::{NodeInputs, NodeProperties};

    /// Build a Rectangle graph (width property = `width_prop`), optionally
    /// wiring a NumberConst emitting `wired_width` into the `width` socket,
    /// evaluate, and return the output cross-section's (x, y) bounds size.
    fn eval_rect_size(width_prop: f64, wired_width: Option<f64>) -> (f64, f64) {
        let mut reg = NodeRegistry::new();
        register_all(&mut reg);
        let mut g = Graph::new();
        let r = g.add_new_node("Rectangle", [0.0, 0.0], &reg).unwrap();
        g.set_property(r, "width", PortValue::Number(width_prop)).unwrap();
        g.set_property(r, "height", PortValue::Number(5.0)).unwrap();
        if let Some(w) = wired_width {
            let nc = g.add_new_node("NumberConst", [-200.0, 0.0], &reg).unwrap();
            g.set_property(nc, "value", PortValue::Number(w)).unwrap();
            let nc_out = g.get(nc).unwrap().output_by_name("out").unwrap().uid;
            let rect_in = g.get(r).unwrap().input_by_name("width").unwrap().uid;
            g.connect(Noodle::new(nc, nc_out, r, rect_in), &reg).unwrap();
        }
        evaluate_all(&mut g, &reg).unwrap().expect_clean();
        let out_uid = g.get(r).unwrap().output_by_name("out").unwrap().uid;
        match g.get(r).unwrap().cached_outputs.get(&out_uid) {
            Some(PortValue::Path2d(cs)) => {
                let s = cs.bounds().size();
                (s.x, s.y)
            }
            other => panic!("expected Path2d output, got {other:?}"),
        }
    }

    #[test]
    fn width_socket_overrides_property() {
        // Property says 4.0 but the wired NumberConst says 8.0 → width 8.0.
        let (w, h) = eval_rect_size(4.0, Some(8.0));
        assert!((w - 8.0).abs() < 1e-6, "width was {w}");
        assert!((h - 5.0).abs() < 1e-6, "height was {h}");
    }

    #[test]
    fn width_falls_back_to_property_when_unwired() {
        let (w, h) = eval_rect_size(4.0, None);
        assert!((w - 4.0).abs() < 1e-6, "width was {w}");
        assert!((h - 5.0).abs() < 1e-6, "height was {h}");
    }

    #[test]
    fn every_optional_input_has_a_bound_property() {
        let mut alloc = SocketUidAlloc::new();
        let tpl = RectangleNode.instantiate(&mut alloc);
        let props = RectangleNode.properties();
        for s in tpl.inputs.iter().filter(|s| s.optional) {
            assert!(
                props
                    .iter()
                    .any(|p| p.bound_input.as_deref() == Some(s.name.as_ref())),
                "no property bound to input '{}'",
                s.name
            );
        }
    }

    /// A project saved before parameter sockets existed restores a
    /// Rectangle instance with only the `out` socket. Evaluation must
    /// still succeed via the property fallback.
    #[test]
    fn legacy_instance_missing_sockets_uses_property_fallback() {
        let mut inst = NodeInstance::new(NodeId(1), "Rectangle", [0.0, 0.0]);
        let mut alloc = SocketUidAlloc::new();
        inst.outputs = InstanceTemplate::builder(&mut alloc)
            .output("out", SocketType::Path2d)
            .build()
            .outputs;
        let mut props = NodeProperties::default();
        props.insert("width", PortValue::Number(6.0));
        props.insert("height", PortValue::Number(3.0));
        let inputs = NodeInputs::default();
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
        match RectangleNode.evaluate(&ctx).unwrap().by_name.get("out").unwrap() {
            PortValue::Path2d(cs) => {
                let s = cs.bounds().size();
                assert!((s.x - 6.0).abs() < 1e-6, "width was {}", s.x);
                assert!((s.y - 3.0).abs() < 1e-6, "height was {}", s.y);
            }
            other => panic!("expected Path2d, got {other:?}"),
        }
    }
}
