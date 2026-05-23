//! NumberConst — emits a constant Number value. Useful as a single source
//! of truth driving multiple Box / Cylinder / Transform inputs.

use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, PropDef,
};
use crate::socket_types::SocketType;

pub struct NumberConstNode;

impl NodeDef for NumberConstNode {
    fn type_id(&self) -> &'static str { "NumberConst" }
    fn display_name(&self) -> &'static str { "Number" }
    fn category(&self) -> &'static str { "Math" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .output("out", SocketType::Number)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![PropDef::new("value", PortValue::Number(1.0)).with_range(-10_000.0, 10_000.0)]
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let v = ctx.properties.number("value", 1.0);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Number(v));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(NumberConstNode);
}
