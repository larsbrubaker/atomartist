//! GraphOutput — declarative output port for a subgraph.
//!
//! Mirrors GraphInput on the other side. When the host graph is wrapped
//! as a `SubgraphNodeDef`, each GraphOutput contributes one output
//! socket named by `name`. Until then, GraphOutput is a passthrough that
//! exposes `out` for downstream debugging.

use std::sync::Arc;

use crate::graph::node::PortValue;
use crate::registry::{
    NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry, PropDef, SocketDef,
};
use crate::socket_types::SocketType;

pub struct GraphOutputNode;

impl NodeDef for GraphOutputNode {
    fn type_id(&self) -> &'static str { "GraphOutput" }
    fn display_name(&self) -> &'static str { "Graph Output" }
    fn category(&self) -> &'static str { "I/O" }

    fn input_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("in", SocketType::Geometry3d)]
    }
    fn output_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("out", SocketType::Geometry3d)]
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("name", PortValue::StringVal(Arc::new("output".into()))),
        ]
    }

    fn evaluate(&self, inputs: &NodeInputs, _props: &NodeProperties) -> Result<NodeOutputs, NodeError> {
        let v = inputs.get("in").clone();
        let mut out = NodeOutputs::default();
        out.set("out", v);
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(GraphOutputNode);
}
