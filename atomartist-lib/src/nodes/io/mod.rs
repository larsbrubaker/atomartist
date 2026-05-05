//! Subgraph I/O markers — GraphInput / GraphOutput.
//!
//! These nodes name and declare the input / output sockets a graph
//! exposes when it's wrapped as a reusable subgraph component. Full
//! runtime SubgraphNodeDef instantiation (which discovers these nodes
//! and synthesizes a NodeDef from the saved graph) is deferred — it
//! requires extending the registry to support owned (non-`'static`)
//! type_id and socket-name strings.

pub mod graph_input_node;
pub mod graph_output_node;

use crate::registry::NodeRegistry;

pub fn register_all(reg: &mut NodeRegistry) {
    graph_input_node::register(reg);
    graph_output_node::register(reg);
}
