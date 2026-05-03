//! Graph subsystem: data structure, evaluation, and undo commands.
//!
//! - `node` — `NodeId`, `SocketId`, `PortValue`, `NodeInstance`
//! - `graph` — `Graph` struct + `GraphError`
//! - `executor` — topological evaluation
//! - `undo_commands` — undo / redo command implementations

pub mod node;
#[allow(clippy::module_inception)]
pub mod graph;
pub mod executor;
pub mod undo_commands;

pub use graph::{Edge, Graph, GraphError};
pub use node::{identity_matrix, NodeId, NodeInstance, PortValue, SocketId};
