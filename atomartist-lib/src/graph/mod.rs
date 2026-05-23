//! Graph subsystem: data structure, evaluation, and undo commands.
//!
//! - `socket` — `SocketUid`, `Socket`, `SocketUidAlloc`
//! - `node` — `NodeId`, `PortValue`, `NodeInstance`
//! - `graph` — `Graph` struct + `GraphError`, `Noodle`, `NoodleEndpoint`
//! - `executor` — topological evaluation
//! - `undo_commands` — undo / redo command implementations

pub mod socket;
pub mod node;
#[allow(clippy::module_inception)]
pub mod graph;
pub mod socket_mutations;
pub mod execution_order;
pub mod executor;
pub mod undo_commands;

pub use graph::{Noodle, NoodleEndpoint, Graph, GraphError};
pub use node::{identity_matrix, NodeId, NodeInstance, PortValue};
pub use socket::{Socket, SocketUid, SocketUidAlloc};
