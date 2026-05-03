//! AtomArtist core library.
//!
//! Contains the visual node-graph engine, typed sockets, node-type registry,
//! 2D / 3D geometry primitives, and serialization. Platform-agnostic — no
//! windowing, no rendering, no file dialogs. The `atomartist-renderer` crate
//! consumes geometry from here for the 3D viewport, and `atomartist-ui`
//! consumes the graph + registry to drive the canvas widget.

pub mod geometry;
pub mod graph;
pub mod nodes;
pub mod registry;
pub mod socket_types;

pub use graph::{Edge, Graph, GraphError, NodeId, NodeInstance, PortValue, SocketId};
pub use registry::{
    NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry, PropDef, SocketDef,
};
pub use socket_types::SocketType;

/// Phase 0 placeholder kept until all callers are gone — `demo-native` /
/// `demo-wasm` still call this in their stub `main`/`start`. Removed once
/// Phase 6 wires up real entry points.
pub fn placeholder() {}
