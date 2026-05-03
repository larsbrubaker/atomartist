//! Node-level data: identifiers, port values, node instances.
//!
//! A `Graph` is composed of `NodeInstance`s wired together by edges. Each
//! node carries:
//!   - a stable `NodeId` (allocated by the graph)
//!   - its registered type id (`&'static str`, looked up in `NodeRegistry`)
//!   - canvas position (free 2D layout decided by the user)
//!   - the current values of its named properties (`PortValue`)
//!   - cached output values (one `PortValue` per declared output socket)
//!
//! The `PortValue` enum is the lingua franca of the graph — every edge
//! carries one, and every property is one. Variants that wrap heap data
//! (`Path2d`, `Geometry3d`, `StringVal`) use `Arc` so downstream nodes share
//! upstream outputs without copying.

use std::sync::Arc;

use manifold_rust::cross_section::CrossSection;
use manifold_rust::types::MeshGL;

use crate::socket_types::SocketType;

/// Stable identifier for a node within a single `Graph`.
///
/// Allocated monotonically; never reused even after a node is removed (so
/// undo commands can re-add a removed node and existing edges referencing
/// the old id remain valid for re-connection).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u64);

/// Identifier for one named socket on one node — the `(node, socket_name)`
/// pair that an edge endpoint refers to. Socket names are `&'static str`
/// because they come from a node-type's static `SocketDef` table.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SocketId {
    pub node: NodeId,
    pub name: &'static str,
}

/// Value flowing along an edge or held in a property.
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

/// One node in a `Graph`. Owns its current property values and (after the
/// executor runs) its cached outputs.
#[derive(Clone, Debug)]
pub struct NodeInstance {
    pub id: NodeId,
    /// Type id matching a `NodeDef` registered in the `NodeRegistry`.
    pub type_id: &'static str,
    /// Position in canvas-space (Y-up — agg-gui convention).
    pub position: [f64; 2],
    /// Current property values, keyed by `PropDef::name`.
    pub properties: std::collections::HashMap<&'static str, PortValue>,
    /// Cached outputs from the most recent successful evaluation, keyed
    /// by `SocketDef::name`. Empty until the executor has run.
    pub cached_outputs: std::collections::HashMap<&'static str, PortValue>,
    /// True when the node's inputs or properties changed since the last
    /// evaluation — set by `Graph::mark_dirty_subtree` and cleared by the
    /// executor after producing fresh outputs.
    pub dirty: bool,
}

impl NodeInstance {
    pub fn new(id: NodeId, type_id: &'static str, position: [f64; 2]) -> Self {
        Self {
            id,
            type_id,
            position,
            properties: Default::default(),
            cached_outputs: Default::default(),
            dirty: true,
        }
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
    fn node_instance_starts_dirty() {
        let n = NodeInstance::new(NodeId(1), "Box", [0.0, 0.0]);
        assert!(n.dirty);
        assert_eq!(n.type_id, "Box");
    }
}
