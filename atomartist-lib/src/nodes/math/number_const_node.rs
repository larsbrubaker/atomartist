//! NumberConst — emits a constant Number value. Useful as a single source
//! of truth driving multiple Box / Cylinder / Transform inputs.

use crate::graph::node::PortValue;
use crate::registry::{
    NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry, PropDef, SocketDef,
};
use crate::socket_types::SocketType;

pub struct NumberConstNode;

impl NodeDef for NumberConstNode {
    fn type_id(&self) -> &'static str { "NumberConst" }
    fn display_name(&self) -> &'static str { "Number" }
    fn category(&self) -> &'static str { "Math" }

    fn input_sockets(&self) -> Vec<SocketDef> { vec![] }
    fn output_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("out", SocketType::Number)]
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![PropDef::new("value", PortValue::Number(1.0)).with_range(-10_000.0, 10_000.0)]
    }

    fn evaluate(&self, _inputs: &NodeInputs, props: &NodeProperties) -> Result<NodeOutputs, NodeError> {
        let v = props.number("value", 1.0);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Number(v));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(NumberConstNode);
}
