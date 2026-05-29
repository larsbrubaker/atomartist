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

use crate::geometry::Geometry3d;
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
    /// Bundle of mesh + per-node matrix + per-node colour (see
    /// [`crate::geometry::Geometry3d`]). Carrying the matrix + colour
    /// alongside the mesh mirrors NodeDesigner's
    /// `socket._data.matrix` / `_data.color` model, so a downstream
    /// node sees the upstream transform without walking the graph.
    Geometry3d(Arc<Geometry3d>),
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

/// Column-major 4×4 multiply: returns `A · B`. Used wherever node ops
/// compose their own transform with an upstream body's matrix (so
/// dragging a gizmo updates a single op's properties and the renderer
/// applies the composed result without re-baking mesh vertices).
pub fn matmul4x4(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut r = [0.0f32; 16];
    for col in 0..4 {
        for row in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += a[k * 4 + row] * b[col * 4 + k];
            }
            r[col * 4 + row] = sum;
        }
    }
    r
}

/// Column-major rotation about a world axis (`0`=X, `1`=Y, `2`=Z) by
/// `angle` radians, right-handed. Translation column is identity — use
/// [`rotate_about_world_axis`] to spin about a point other than the
/// world origin.
fn axis_rotation_matrix(axis_index: u8, angle: f32) -> [f32; 16] {
    let (s, c) = angle.sin_cos();
    match axis_index {
        // X: Y→Z
        0 => [
            1.0, 0.0, 0.0, 0.0,
            0.0, c, s, 0.0,
            0.0, -s, c, 0.0,
            0.0, 0.0, 0.0, 1.0,
        ],
        // Y: Z→X
        1 => [
            c, 0.0, -s, 0.0,
            0.0, 1.0, 0.0, 0.0,
            s, 0.0, c, 0.0,
            0.0, 0.0, 0.0, 1.0,
        ],
        // Z: X→Y (default for any out-of-range index)
        _ => [
            c, s, 0.0, 0.0,
            -s, c, 0.0, 0.0,
            0.0, 0.0, 1.0, 0.0,
            0.0, 0.0, 0.0, 1.0,
        ],
    }
}

/// Pre-multiply a world-space rotation of `angle` radians about the
/// world `axis_index` axis (`0`=X, `1`=Y, `2`=Z) passing through
/// `center` onto `start`.
///
/// The rendered `Body.matrix` is `node_matrix · upstream`
/// (`registry::geometry_props::compose_with_upstream`). Applying the
/// rotation on the **left** (`R · node_matrix · upstream`) rotates the
/// composed body about `center` in *world* space while leaving the
/// upstream factor untouched — so a gizmo drag spins the body about a
/// world axis regardless of any upstream transform. `R` is
/// `T(center) · R_axis(angle) · T(-center)`, built directly in
/// column-major form: the rotation block is `R_axis`, and the
/// translation column is `center - R_axis · center` so `center` stays
/// fixed.
///
/// Used by the rotate gizmo (3-axis corner control) and by the
/// type-an-angle field, which share this so a dragged and a typed
/// rotation land identically.
pub fn rotate_about_world_axis(
    start: &[f32; 16],
    center: [f32; 3],
    axis_index: u8,
    angle: f32,
) -> [f32; 16] {
    let mut world_rot = axis_rotation_matrix(axis_index, angle);
    // R_axis · center (column-major element (row, col) = m[col*4 + row]).
    let rc = [
        world_rot[0] * center[0] + world_rot[4] * center[1] + world_rot[8] * center[2],
        world_rot[1] * center[0] + world_rot[5] * center[1] + world_rot[9] * center[2],
        world_rot[2] * center[0] + world_rot[6] * center[1] + world_rot[10] * center[2],
    ];
    // Translation that keeps `center` fixed: center - R·center.
    world_rot[12] = center[0] - rc[0];
    world_rot[13] = center[1] - rc[1];
    world_rot[14] = center[2] - rc[2];
    matmul4x4(&world_rot, start)
}

/// Wrap `angle` into the half-open interval `(-π, π]`. Turns a raw
/// `current - anchor` pointer-angle difference into the shortest signed
/// rotation, so a rotate drag takes the short way round and never
/// snaps a near-full turn when the pointer crosses the ±π atan2 seam.
pub fn normalize_angle(angle: f32) -> f32 {
    use std::f32::consts::PI;
    let two_pi = 2.0 * PI;
    let mut a = angle % two_pi;
    if a > PI {
        a -= two_pi;
    } else if a <= -PI {
        a += two_pi;
    }
    a
}

/// Angle (radians) of `hit` about `center`, measured in the plane
/// perpendicular to the world `axis_index` axis. Mirrors MatterCAD's
/// `RotateCornerControl.Mouse3DInfo.GetAngleForAxis`: X uses
/// `atan2(dz, dy)`, Y uses `atan2(dx, dz)`, Z uses `atan2(dy, dx)`.
/// The per-axis ordering is chosen so an increasing angle corresponds
/// to a right-handed (positive) rotation about that axis — i.e. the
/// grabbed point follows the cursor when this feeds
/// [`rotate_about_world_axis`].
pub fn angle_on_axis_plane(hit: [f32; 3], center: [f32; 3], axis_index: u8) -> f32 {
    let d = [hit[0] - center[0], hit[1] - center[1], hit[2] - center[2]];
    match axis_index {
        0 => d[2].atan2(d[1]),
        1 => d[0].atan2(d[2]),
        _ => d[1].atan2(d[0]),
    }
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
        use manifold_rust::types::MeshGL;
        let g1 = Arc::new(Geometry3d::from_mesh(Arc::new(MeshGL::default())));
        let g2 = Arc::new(Geometry3d::from_mesh(Arc::new(MeshGL::default())));
        let geo_a = PortValue::Geometry3d(g1.clone());
        let geo_a_clone = PortValue::Geometry3d(g1.clone());
        let geo_b = PortValue::Geometry3d(g2);
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

    /// Transform a point through a column-major 4×4 — test-only helper.
    fn xform(m: &[f32; 16], p: [f32; 3]) -> [f32; 3] {
        [
            m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
            m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
            m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
        ]
    }

    fn approx(a: [f32; 3], b: [f32; 3]) -> bool {
        (a[0] - b[0]).abs() < 1e-4 && (a[1] - b[1]).abs() < 1e-4 && (a[2] - b[2]).abs() < 1e-4
    }

    #[test]
    fn rotate_about_world_axis_quarter_turns_map_known_points() {
        let q = std::f32::consts::FRAC_PI_2;
        // X axis: +Y → +Z.
        let mx = rotate_about_world_axis(&identity_matrix(), [0.0, 0.0, 0.0], 0, q);
        assert!(approx(xform(&mx, [0.0, 1.0, 0.0]), [0.0, 0.0, 1.0]));
        // Y axis: +Z → +X.
        let my = rotate_about_world_axis(&identity_matrix(), [0.0, 0.0, 0.0], 1, q);
        assert!(approx(xform(&my, [0.0, 0.0, 1.0]), [1.0, 0.0, 0.0]));
        // Z axis: +X → +Y.
        let mz = rotate_about_world_axis(&identity_matrix(), [0.0, 0.0, 0.0], 2, q);
        assert!(approx(xform(&mz, [1.0, 0.0, 0.0]), [0.0, 1.0, 0.0]));
    }

    #[test]
    fn rotate_about_world_axis_keeps_center_fixed() {
        let center = [3.0, -2.0, 5.0];
        for axis in 0..3u8 {
            let m = rotate_about_world_axis(&identity_matrix(), center, axis, 1.1);
            assert!(
                approx(xform(&m, center), center),
                "axis {axis} moved its own center",
            );
        }
    }

    #[test]
    fn rotate_about_world_axis_zero_angle_is_identity() {
        let start: [f32; 16] = [
            2.0, 0.0, 0.0, 0.0,
            0.0, 3.0, 0.0, 0.0,
            0.0, 0.0, 4.0, 0.0,
            5.0, 6.0, 7.0, 1.0,
        ];
        let out = rotate_about_world_axis(&start, [1.0, 2.0, 3.0], 1, 0.0);
        for i in 0..16 {
            assert!((out[i] - start[i]).abs() < 1e-5, "entry {i} drifted");
        }
    }

    #[test]
    fn rotate_about_world_axis_premultiplies_into_world_space() {
        // Body translated to (10,0,0); +90° about world Z through the
        // origin must swing its origin to (0,10,0) — a world-axis spin,
        // not a spin about the body's own local origin.
        let start: [f32; 16] = [
            1.0, 0.0, 0.0, 0.0,
            0.0, 1.0, 0.0, 0.0,
            0.0, 0.0, 1.0, 0.0,
            10.0, 0.0, 0.0, 1.0,
        ];
        let out = rotate_about_world_axis(&start, [0.0, 0.0, 0.0], 2, std::f32::consts::FRAC_PI_2);
        assert!(approx(xform(&out, [0.0, 0.0, 0.0]), [0.0, 10.0, 0.0]));
    }

    #[test]
    fn angle_on_axis_plane_matches_per_axis_atan2() {
        let c = [1.0, 1.0, 1.0];
        // Z plane: +X direction → 0, +Y direction → +π/2.
        assert!(angle_on_axis_plane([2.0, 1.0, 1.0], c, 2).abs() < 1e-5);
        assert!((angle_on_axis_plane([1.0, 2.0, 1.0], c, 2) - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        // X plane: +Y direction → 0 (atan2(dz,dy)), +Z direction → +π/2.
        assert!(angle_on_axis_plane([1.0, 2.0, 1.0], c, 0).abs() < 1e-5);
        assert!((angle_on_axis_plane([1.0, 1.0, 2.0], c, 0) - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        // Y plane: +Z direction → 0 (atan2(dx,dz)), +X direction → +π/2.
        assert!(angle_on_axis_plane([1.0, 1.0, 2.0], c, 1).abs() < 1e-5);
        assert!((angle_on_axis_plane([2.0, 1.0, 1.0], c, 1) - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
    }

    /// The measure (`angle_on_axis_plane`) and the apply
    /// (`rotate_about_world_axis`) must agree in direction for every
    /// axis: rotating by the measured angle of a point lands that point
    /// on the axis's "zero" ray. This is what makes the grabbed handle
    /// follow the cursor on all three rings.
    #[test]
    fn measure_and_apply_agree_per_axis() {
        for axis in 0..3u8 {
            let center = [0.0, 0.0, 0.0];
            // A point off the zero-ray in this axis's plane.
            let p = match axis {
                0 => [0.0, 1.0, 1.0],   // X plane (y,z)
                1 => [1.0, 0.0, 1.0],   // Y plane (z,x)
                _ => [1.0, 1.0, 0.0],   // Z plane (x,y)
            };
            let a = angle_on_axis_plane(p, center, axis);
            // Rotating by -a should bring the point onto the zero ray
            // (angle 0) for that axis.
            let m = rotate_about_world_axis(&identity_matrix(), center, axis, -a);
            let rotated = xform(&m, p);
            assert!(
                angle_on_axis_plane(rotated, center, axis).abs() < 1e-4,
                "axis {axis}: measure/apply disagree",
            );
        }
    }
}
