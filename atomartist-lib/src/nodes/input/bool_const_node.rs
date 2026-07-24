//! BoolConst — emits a constant Boolean value. A single source of truth for
//! toggling downstream boolean inputs (e.g. feature flags on operation nodes).
//!
//! Part of the `Input` category (see `super`), alongside the Number, String,
//! and Color constant nodes.
//!
//! Not migrated to the declarative `ParamSet` schema: this node has **no
//! input sockets** and a single `value` property that `evaluate` reads
//! directly, so there is no socket/property/reader triple for `ParamSet`
//! to collapse — it would only add indirection.

use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EditorKind, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, PropDef,
};
use crate::socket_types::SocketType;

pub struct BoolConstNode;

impl NodeDef for BoolConstNode {
    fn type_id(&self) -> &'static str { "BoolConst" }
    fn display_name(&self) -> &'static str { "Boolean" }
    fn category(&self) -> &'static str { "Input" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .output("out", SocketType::Bool)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![PropDef::new("value", PortValue::Bool(true)).with_editor(EditorKind::Toggle)]
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let v = ctx.properties.bool_("value", true);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Bool(v));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(BoolConstNode);
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
        let def = reg.get("BoolConst").expect("BoolConst registered");
        assert_eq!(def.category(), "Input");

        let mut alloc = SocketUidAlloc::new();
        let tpl = def.instantiate(&mut alloc);
        let mut inst = NodeInstance::new(NodeId(1), def.type_id().to_string(), [0.0, 0.0]);
        inst.inputs = tpl.inputs;
        inst.outputs = tpl.outputs;

        let mut props = NodeProperties::default();
        props.insert("value", PortValue::Bool(false));
        let inputs = NodeInputs::default();
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };

        let outs = def.evaluate(&ctx).unwrap();
        match outs.by_name.get("out").unwrap() {
            PortValue::Bool(b) => assert_eq!(*b, false),
            other => panic!("expected Bool, got {other:?}"),
        }
    }
}
