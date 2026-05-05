//! GraphInput — declarative input port for a subgraph.
//!
//! When the host graph is wrapped as a `SubgraphNodeDef` (future), each
//! GraphInput contributes one input socket on the resulting subgraph
//! node, named by the `name` property and typed Geometry3d. Until that
//! runtime instantiation lands, GraphInput acts as a passthrough that
//! emits its `default_value` property — useful as a visible marker for
//! "this is a parameter" in a graph that's evolving toward subgraph form.

use std::sync::Arc;

use crate::graph::node::PortValue;
use crate::registry::{
    NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry, PropDef, SocketDef,
};
use crate::socket_types::SocketType;

pub struct GraphInputNode;

impl NodeDef for GraphInputNode {
    fn type_id(&self) -> &'static str { "GraphInput" }
    fn display_name(&self) -> &'static str { "Graph Input" }
    fn category(&self) -> &'static str { "I/O" }

    fn input_sockets(&self) -> Vec<SocketDef> { vec![] }
    fn output_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("out", SocketType::Geometry3d)]
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("name", PortValue::StringVal(Arc::new("input".into()))),
        ]
    }

    fn evaluate(&self, _inputs: &NodeInputs, _props: &NodeProperties) -> Result<NodeOutputs, NodeError> {
        // Standalone (non-subgraph) usage emits None — the parent has
        // nothing to inject. When this node is hosted inside a Subgraph,
        // the SubgraphNodeDef::evaluate sets `out` directly via cached
        // override before calling the executor.
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::None);
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(GraphInputNode);
}
