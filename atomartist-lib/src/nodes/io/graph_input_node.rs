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
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, PropDef,
};
use crate::socket_types::SocketType;

pub struct GraphInputNode;

impl NodeDef for GraphInputNode {
    fn type_id(&self) -> &'static str { "GraphInput" }
    fn display_name(&self) -> &'static str { "Graph Input" }
    fn category(&self) -> &'static str { "I/O" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .output("out", SocketType::Geometry3d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("name", PortValue::StringVal(Arc::new("input".into()))),
            // Set by SubgraphNodeDef::evaluate before running the
            // executor on the cloned template; standalone (non-subgraph)
            // usage leaves it `None` and the node emits `None`.
            PropDef::new("_injected", PortValue::None),
        ]
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let value = ctx.properties.get("_injected").clone();
        let mut out = NodeOutputs::default();
        out.set("out", value);
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(GraphInputNode);
}
