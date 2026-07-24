//! ColorConst — emits a constant RGBA color. Feeds Color-typed inputs such as
//! the extrude/primitive body color without wiring a full material graph.
//!
//! Part of the `Input` category (see `super`), alongside the Number, Boolean,
//! and String constant nodes.
//!
//! Not migrated to the declarative `ParamSet` schema: this node has **no
//! input sockets** and a single `value` property that `evaluate` reads
//! directly, so there is no socket/property/reader triple for `ParamSet`
//! to collapse.

use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EditorKind, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, PropDef,
};
use crate::socket_types::SocketType;

/// Opaque white — mirrors the default color used by geometry nodes
/// (see `ops_3d::extrude_node`) so a fresh Color node reads neutrally.
const DEFAULT_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

pub struct ColorConstNode;

impl NodeDef for ColorConstNode {
    fn type_id(&self) -> &'static str { "ColorConst" }
    fn display_name(&self) -> &'static str { "Color" }
    fn category(&self) -> &'static str { "Input" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .output("out", SocketType::Color)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("value", PortValue::Color(DEFAULT_COLOR))
                .with_editor(EditorKind::ColorPicker),
        ]
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let v = ctx.properties.color("value", DEFAULT_COLOR);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Color(v));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(ColorConstNode);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::{NodeId, NodeInstance};
    use crate::registry::{NodeInputs, NodeProperties};

    #[test]
    fn emits_property_value() {
        let mut reg = NodeRegistry::new();
        register(&mut reg);
        let def = reg.get("ColorConst").expect("ColorConst registered");
        assert_eq!(def.category(), "Input");

        let mut alloc = SocketUidAlloc::new();
        let tpl = def.instantiate(&mut alloc);
        let mut inst = NodeInstance::new(NodeId(1), def.type_id().to_string(), [0.0, 0.0]);
        inst.inputs = tpl.inputs;
        inst.outputs = tpl.outputs;

        let mut props = NodeProperties::default();
        props.insert("value", PortValue::Color([0.25, 0.5, 0.75, 1.0]));
        let inputs = NodeInputs::default();
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };

        let outs = def.evaluate(&ctx).unwrap();
        match outs.by_name.get("out").unwrap() {
            PortValue::Color(c) => assert_eq!(*c, [0.25, 0.5, 0.75, 1.0]),
            other => panic!("expected Color, got {other:?}"),
        }
    }
}
