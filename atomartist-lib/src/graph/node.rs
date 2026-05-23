//! Node-level data: identifiers, port values, node instances.
//!
//! A `Graph` is composed of `NodeInstance`s wired together by noodles. Each
//! node owns its socket layout (`inputs`, `outputs`) and its property
//! values; the type's `NodeDef` is the factory that mints the initial
//! socket list and exposes connection-time behavior, but it does not
//! answer "what sockets do I have?" once the instance exists.
//!
//! The `PortValue` enum is the lingua franca of the graph — every noodle
//! carries one, and every property is one. Variants that wrap heap data
//! (`Path2d`, `Geometry3d`, `StringVal`) use `Arc` so downstream nodes share
//! upstream outputs without copying.

use std::collections::HashMap;
use std::sync::Arc;

use manifold_rust::cross_section::CrossSection;
use manifold_rust::types::MeshGL;

use crate::graph::socket::{Socket, SocketUid};
use crate::socket_types::SocketType;

/// Stable identifier for a node within a single `Graph`.
///
/// Allocated monotonically; never reused even after a node is removed (so
/// undo commands can re-add a removed node and existing noodles referencing
/// the old id remain valid for re-connection).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u64);

/// Value flowing along a noodle or held in a property.
///
/// Heap-backed variants are `Arc`-wrapped so cloning a `PortValue` is cheap
/// and downstream nodes share the producer's allocation. `PartialEq` on
/// `Arc`-wrapped variants is pointer-identity (`Arc::ptr_eq`); this makes
/// dirty-tracking O(1) and avoids deep mesh comparisons. Two distinct
/// allocations with identical contents are considered "different" for
/// dirty-checking purposes — which is correct because a producer that
/// re-evaluates always allocates anew.
#[derive(Clone, Debug)]
pub enum PortValue {
    None,
    Number(f64),
    Bool(bool),
    StringVal(Arc<String>),
    /// Linear RGBA, components in 0..=1.
    Color([f32; 4]),
    /// Column-major 4×4 matrix (matches OpenGL / wgpu convention).
    Matrix4x4([f32; 16]),
    Path2d(Arc<CrossSection>),
    Geometry3d(Arc<MeshGL>),
}

impl PortValue {
    /// Logical type of this value, used to validate connections.
    pub fn socket_type(&self) -> SocketType {
        match self {
            PortValue::None => SocketType::None,
            PortValue::Number(_) => SocketType::Number,
            PortValue::Bool(_) => SocketType::Bool,
            PortValue::StringVal(_) => SocketType::StringVal,
            PortValue::Color(_) => SocketType::Color,
            PortValue::Matrix4x4(_) => SocketType::Matrix4x4,
            PortValue::Path2d(_) => SocketType::Path2d,
            PortValue::Geometry3d(_) => SocketType::Geometry3d,
        }
    }
}

impl PartialEq for PortValue {
    fn eq(&self, other: &Self) -> bool {
        use PortValue::*;
        match (self, other) {
            (None, None) => true,
            (Number(a), Number(b)) => a == b,
            (Bool(a), Bool(b)) => a == b,
            (StringVal(a), StringVal(b)) => Arc::ptr_eq(a, b) || **a == **b,
            (Color(a), Color(b)) => a == b,
            (Matrix4x4(a), Matrix4x4(b)) => a == b,
            // Heap-backed mesh / path: pointer identity. Cheap and correct
            // for dirty-tracking — see the doc comment on `PortValue`.
            (Path2d(a), Path2d(b)) => Arc::ptr_eq(a, b),
            (Geometry3d(a), Geometry3d(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

/// Convenience constructor for the identity matrix in column-major layout.
pub fn identity_matrix() -> [f32; 16] {
    [
        1.0, 0.0, 0.0, 0.0,
        0.0, 1.0, 0.0, 0.0,
        0.0, 0.0, 1.0, 0.0,
        0.0, 0.0, 0.0, 1.0,
    ]
}

/// One node in a `Graph`. Owns its socket layout and the current values of
/// its named properties; the executor caches the most recent evaluated
/// outputs in `cached_outputs`.
#[derive(Clone, Debug)]
pub struct NodeInstance {
    pub id: NodeId,
    /// Type id matching a `NodeDef` registered in the `NodeRegistry`.
    /// `Arc<str>` (not `&'static str`) so future user-defined node types
    /// can carry runtime-allocated identifiers without lifetime acrobatics.
    pub type_id: Arc<str>,
    /// Position in canvas-space (Y-up — agg-gui convention).
    pub position: [f64; 2],
    /// Input sockets, in display order. Order is meaningful (drag-reorder
    /// is a Vec permutation). Noodles reference these by `Socket::uid`.
    pub inputs: Vec<Socket>,
    /// Output sockets, in display order. Same ordering rules as `inputs`.
    pub outputs: Vec<Socket>,
    /// Current property values, keyed by `PropDef::name`. `Arc<str>` keys
    /// so dynamic nodes can introduce runtime property names — symmetric
    /// with the socket model.
    pub properties: HashMap<Arc<str>, PortValue>,
    /// Cached outputs from the most recent successful evaluation, keyed
    /// by the producing socket's `SocketUid`. Empty until the executor
    /// has run. Survives renames (uid is stable identity).
    pub cached_outputs: HashMap<SocketUid, PortValue>,
    /// True when the node's inputs or properties changed since the last
    /// evaluation — set by `Graph::mark_dirty_subtree` and cleared by the
    /// executor after producing fresh outputs.
    pub dirty: bool,
}

impl NodeInstance {
    /// Bare-bones constructor — sockets default to empty. Real construction
    /// goes through `Graph::add_node_with_def` which calls
    /// `NodeDef::instantiate` to populate sockets and initial properties.
    pub fn new(id: NodeId, type_id: impl Into<Arc<str>>, position: [f64; 2]) -> Self {
        Self {
            id,
            type_id: type_id.into(),
            position,
            inputs: Vec::new(),
            outputs: Vec::new(),
            properties: HashMap::new(),
            cached_outputs: HashMap::new(),
            dirty: true,
        }
    }

    /// Look up an input socket by name. Returns `None` when no socket has
    /// that name. Empty-named slots (used by dynamic-input nodes for the
    /// trailing placeholder) are matched too.
    pub fn input_by_name(&self, name: &str) -> Option<&Socket> {
        self.inputs.iter().find(|s| &*s.name == name)
    }

    /// Look up an input socket by uid.
    pub fn input_by_uid(&self, uid: SocketUid) -> Option<&Socket> {
        self.inputs.iter().find(|s| s.uid == uid)
    }

    /// Index of an input socket by uid, for in-place mutation.
    pub fn input_index_by_uid(&self, uid: SocketUid) -> Option<usize> {
        self.inputs.iter().position(|s| s.uid == uid)
    }

    /// Look up an output socket by name.
    pub fn output_by_name(&self, name: &str) -> Option<&Socket> {
        self.outputs.iter().find(|s| &*s.name == name)
    }

    /// Look up an output socket by uid.
    pub fn output_by_uid(&self, uid: SocketUid) -> Option<&Socket> {
        self.outputs.iter().find(|s| s.uid == uid)
    }

    /// Index of an output socket by uid, for in-place mutation.
    pub fn output_index_by_uid(&self, uid: SocketUid) -> Option<usize> {
        self.outputs.iter().position(|s| s.uid == uid)
    }

    /// First input socket whose `socket_type` matches `ty` exactly. The
    /// ordering matches the canvas display (which is `inputs` order).
    pub fn input_by_type(&self, ty: SocketType) -> Option<&Socket> {
        self.inputs.iter().find(|s| s.socket_type == ty)
    }

    /// First output socket whose `socket_type` matches `ty` exactly.
    pub fn output_by_type(&self, ty: SocketType) -> Option<&Socket> {
        self.outputs.iter().find(|s| s.socket_type == ty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_value_socket_type_matches_variant() {
        assert_eq!(PortValue::None.socket_type(), SocketType::None);
        assert_eq!(PortValue::Number(3.0).socket_type(), SocketType::Number);
        assert_eq!(PortValue::Bool(true).socket_type(), SocketType::Bool);
        assert_eq!(
            PortValue::Color([1.0, 0.0, 0.0, 1.0]).socket_type(),
            SocketType::Color
        );
        assert_eq!(
            PortValue::Matrix4x4(identity_matrix()).socket_type(),
            SocketType::Matrix4x4
        );
    }

    #[test]
    fn port_value_eq_pointer_identity_for_arc_variants() {
        let mesh1 = Arc::new(MeshGL::default());
        let mesh2 = Arc::new(MeshGL::default());
        let geo_a = PortValue::Geometry3d(mesh1.clone());
        let geo_a_clone = PortValue::Geometry3d(mesh1.clone());
        let geo_b = PortValue::Geometry3d(mesh2);
        assert_eq!(geo_a, geo_a_clone, "same Arc → equal");
        assert_ne!(geo_a, geo_b, "distinct Arcs (even with equal contents) → not equal");
    }

    #[test]
    fn port_value_eq_structural_for_simple_types() {
        assert_eq!(PortValue::Number(1.5), PortValue::Number(1.5));
        assert_ne!(PortValue::Number(1.5), PortValue::Number(2.5));
        assert_eq!(PortValue::Bool(true), PortValue::Bool(true));
    }

    #[test]
    fn port_value_eq_string_compares_content_with_arc_fast_path() {
        let s1 = Arc::new(String::from("hello"));
        let s1_clone = s1.clone();
        let s2 = Arc::new(String::from("hello"));
        let s3 = Arc::new(String::from("world"));
        assert_eq!(PortValue::StringVal(s1.clone()), PortValue::StringVal(s1_clone));
        assert_eq!(PortValue::StringVal(s1), PortValue::StringVal(s2));
        assert_ne!(PortValue::StringVal(Arc::new("a".into())), PortValue::StringVal(s3));
    }

    #[test]
    fn node_instance_starts_dirty_and_empty() {
        let n = NodeInstance::new(NodeId(1), "Box", [0.0, 0.0]);
        assert!(n.dirty);
        assert_eq!(&*n.type_id, "Box");
        assert!(n.inputs.is_empty());
        assert!(n.outputs.is_empty());
    }

    #[test]
    fn input_lookups_round_trip() {
        let mut n = NodeInstance::new(NodeId(1), "Box", [0.0, 0.0]);
        n.inputs.push(Socket::new(SocketUid(7), "size", SocketType::Number, false));
        assert_eq!(n.input_by_name("size").unwrap().uid, SocketUid(7));
        assert_eq!(n.input_by_uid(SocketUid(7)).unwrap().name.as_ref(), "size");
        assert_eq!(n.input_index_by_uid(SocketUid(7)), Some(0));
        assert!(n.input_by_name("missing").is_none());
    }
}
