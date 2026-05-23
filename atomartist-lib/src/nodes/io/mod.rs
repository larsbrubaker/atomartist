//! Subgraph I/O markers — `GraphInput` only.
//!
//! Subgraph inputs are declared with explicit `GraphInput` nodes (each
//! one's `name` property names the published input port). Subgraph
//! *outputs* are no longer declared with a dedicated node — the unified
//! [`OutputNode`](super::output_node) plays both roles (viewport display
//! anchor at top level, output-port declarator inside a subgraph
//! template). Its mirror output sockets become the subgraph's published
//! outputs; see [`super::subgraph_node`].

pub mod graph_input_node;

use crate::registry::NodeRegistry;

pub fn register_all(reg: &mut NodeRegistry) {
    graph_input_node::register(reg);
}
