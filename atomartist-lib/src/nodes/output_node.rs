//! Output — the terminal display anchor. Whatever Geometry3d flows into
//! the Output node is what the 3D viewport renders. It has a single
//! `in` input (Geometry3d) and mirrors that value out so downstream
//! debugging tools can re-tap it. Display semantics live in
//! `AppState::pick_display_mesh`.

use crate::graph::node::PortValue;
use crate::registry::{
    NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry, SocketDef,
};
use crate::socket_types::SocketType;

pub struct OutputNode;

impl NodeDef for OutputNode {
    fn type_id(&self) -> &'static str { "Output" }
    fn display_name(&self) -> &'static str { "Output" }
    fn category(&self) -> &'static str { "Output" }

    fn input_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("in", SocketType::Geometry3d)]
    }
    fn output_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("out", SocketType::Geometry3d)]
    }

    fn evaluate(&self, inputs: &NodeInputs, _props: &NodeProperties) -> Result<NodeOutputs, NodeError> {
        let v = inputs.get("in").clone();
        let mut out = NodeOutputs::default();
        out.set("out", v);
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(OutputNode);
}
