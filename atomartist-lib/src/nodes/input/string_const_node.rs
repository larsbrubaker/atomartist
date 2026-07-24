//! StringConst — emits a constant text value. Feeds string-typed inputs such
//! as labels, names, or file references on downstream nodes.
//!
//! Part of the `Input` category (see `super`), alongside the Number, Boolean,
//! and Color constant nodes.
//!
//! Not migrated to the declarative `ParamSet` schema: this node has **no
//! input sockets** and a single `value` property that `evaluate` reads
//! directly, so there is no socket/property/reader triple for `ParamSet`
//! to collapse.

use std::sync::Arc;

use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EditorKind, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, PropDef,
};
use crate::socket_types::SocketType;

pub struct StringConstNode;

impl NodeDef for StringConstNode {
    fn type_id(&self) -> &'static str { "StringConst" }
    fn display_name(&self) -> &'static str { "String" }
    fn category(&self) -> &'static str { "Input" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .output("out", SocketType::StringVal)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("value", PortValue::StringVal(Arc::new(String::new())))
                .with_editor(EditorKind::StringSingleLine),
        ]
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        // No dedicated string accessor on NodeProperties; read the raw
        // PortValue and fall back to an empty string when unset/mistyped.
        let v = match ctx.properties.get("value") {
            PortValue::StringVal(s) => s.clone(),
            _ => Arc::new(String::new()),
        };
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::StringVal(v));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(StringConstNode);
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
        let def = reg.get("StringConst").expect("StringConst registered");
        assert_eq!(def.category(), "Input");

        let mut alloc = SocketUidAlloc::new();
        let tpl = def.instantiate(&mut alloc);
        let mut inst = NodeInstance::new(NodeId(1), def.type_id().to_string(), [0.0, 0.0]);
        inst.inputs = tpl.inputs;
        inst.outputs = tpl.outputs;

        let mut props = NodeProperties::default();
        props.insert("value", PortValue::StringVal(Arc::new("hello".to_string())));
        let inputs = NodeInputs::default();
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };

        let outs = def.evaluate(&ctx).unwrap();
        match outs.by_name.get("out").unwrap() {
            PortValue::StringVal(s) => assert_eq!(s.as_str(), "hello"),
            other => panic!("expected StringVal, got {other:?}"),
        }
    }
}
