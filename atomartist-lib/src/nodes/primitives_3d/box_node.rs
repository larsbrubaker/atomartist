//! Box primitive node — generates an axis-aligned cuboid centered at origin.

use std::sync::Arc;

use crate::geometry::generate_box;
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    wrap_mesh, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, ParamSet,
    PropDef,
};
use crate::socket_types::SocketType;

pub struct BoxNode;

/// The Box node's parameter schema. The shared `Color` / `Matrix` params
/// (via [`ParamSet::geometry`]) lead — rendered as the first two rows and
/// resolved by [`wrap_mesh`] — with the three scalar dimensions following.
/// Every param gets an optional input socket (the "socket-or-property"
/// shape); the capitalized socket names (`Width` / `Height` / `Depth`) are
/// the chosen display convention (matching Extrude / Cylinder), set via
/// `socket_named` since the property keys are lowercase.
fn params() -> ParamSet {
    ParamSet::geometry()
        .number("width", "Width", 20.0, 0.001..=10_000.0)
        .socket_named("Width")
        .number("height", "Height", 20.0, 0.001..=10_000.0)
        .socket_named("Height")
        .number("depth", "Depth", 20.0, 0.001..=10_000.0)
        .socket_named("Depth")
}

impl NodeDef for BoxNode {
    fn type_id(&self) -> &'static str { "Box" }
    fn display_name(&self) -> &'static str { "Box" }
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
        let r = ps.reader(ctx);
        let w = r.number("width");
        let h = r.number("height");
        let d = r.number("depth");
        let mesh = generate_box(w, h, d);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Geometry3d(Arc::new(wrap_mesh(ctx, mesh))));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(BoxNode);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::{NodeId, NodeInstance};
    use crate::registry::{NodeInputs, NodeProperties};

    /// Build a (NodeInstance, NodeInputs, NodeProperties) fixture for
    /// BoxNode with by-name input overrides + properties.
    fn make_ctx_fixture(
        named_inputs: &[(&str, PortValue)],
        named_props: &[(&str, PortValue)],
    ) -> (NodeInstance, NodeInputs, NodeProperties) {
        let mut alloc = SocketUidAlloc::new();
        let tpl = BoxNode.instantiate(&mut alloc);
        let mut inst = NodeInstance::new(NodeId(1), "Box", [0.0, 0.0]);
        inst.inputs = tpl.inputs;
        inst.outputs = tpl.outputs;
        let mut inputs = NodeInputs::default();
        for (name, value) in named_inputs {
            let uid = inst.input_by_name(name).unwrap().uid;
            inputs.insert(uid, value.clone());
        }
        let mut props = NodeProperties::default();
        for (name, value) in named_props {
            props.insert(*name, value.clone());
        }
        (inst, inputs, props)
    }

    /// Y-extent of the emitted box body's mesh (height drives the Y axis
    /// in `generate_box`).
    fn y_extent(out: &NodeOutputs) -> f32 {
        match out.by_name.get("out").unwrap() {
            PortValue::Geometry3d(g) => {
                let m = &g.first().unwrap().mesh;
                let stride = m.num_prop as usize;
                let nv = m.vert_properties.len() / stride;
                let mut mn = f32::INFINITY;
                let mut mx = f32::NEG_INFINITY;
                for i in 0..nv {
                    let y = m.vert_properties[i * stride + 1];
                    if y < mn { mn = y; }
                    if y > mx { mx = y; }
                }
                mx - mn
            }
            _ => panic!("expected Geometry3d output"),
        }
    }

    #[test]
    fn every_optional_input_has_a_bound_property() {
        let mut alloc = SocketUidAlloc::new();
        let tpl = BoxNode.instantiate(&mut alloc);
        let props = BoxNode.properties();
        for input in tpl.inputs.iter().filter(|s| s.optional) {
            let matched = props.iter().any(|p| {
                p.bound_input.as_ref().map(|b| b.as_ref()) == Some(input.name.as_ref())
            });
            assert!(matched, "no property bound to input '{}'", input.name);
        }
    }

    #[test]
    fn wired_height_input_wins_over_property() {
        // Wire Height=30 while the stored property says 20 — the socket
        // value must drive the Y extent (30).
        let (inst, inputs, props) = make_ctx_fixture(
            &[("Height", PortValue::Number(30.0))],
            &[("height", PortValue::Number(20.0))],
        );
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
        let out = BoxNode.evaluate(&ctx).unwrap();
        assert!((y_extent(&out) - 30.0).abs() < 1e-4, "wired height should win");
    }

    #[test]
    fn unconnected_height_falls_back_to_property() {
        let (inst, inputs, props) =
            make_ctx_fixture(&[], &[("height", PortValue::Number(12.0))]);
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
        let out = BoxNode.evaluate(&ctx).unwrap();
        assert!((y_extent(&out) - 12.0).abs() < 1e-4, "property should feed height");
    }
}
