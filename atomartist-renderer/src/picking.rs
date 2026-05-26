//! Ray-mesh picking for the 3-D viewport.
//!
//! Provides:
//!   - [`raycast_mesh`] — Möller-Trumbore against every triangle of a
//!     `MeshGL`, returning the closest hit point in world space.
//!   - [`project_to_view_plane`] — legacy fallback used when the
//!     cursor misses the mesh and we just need a sensible orbit
//!     pivot.
//!   - [`HitPlane`] + [`resolve_pivot_or_fallback`] — port of
//!     MatterCAD's `SceneInteractionPicking.TryResolveSceneOrFallbackHit`.
//!     Computes a world-space pivot point AND an interaction plane
//!     (a plane perpendicular to the screen-centre view ray through
//!     the pivot, or the bed plane on an empty scene). Pan/rotate/
//!     wheel-zoom all share this so the interaction stays anchored
//!     to a real world point under the cursor.
//!
//! All helpers run on the CPU. For the mesh sizes AtomArtist deals with
//! today (a few thousand triangles), brute-force triangle iteration is fine
//! — every UI test point is sub-millisecond. An AABB / BVH accelerator is
//! a future optimisation when scenes grow.

use glam::{Mat4, Vec3};
use manifold_rust::types::MeshGL;

use atomartist_lib::geometry::Geometry3d;
use atomartist_lib::graph::node::NodeId;

use crate::camera::OrbitCamera;

/// World-space ray intersection with the closest triangle of `mesh`.
/// Returns the hit point in world coordinates, or `None` if the ray misses
/// every triangle. `origin` and `direction` come from
/// [`OrbitCamera::screen_to_ray`]; `direction` should be unit-length.
pub fn raycast_mesh(
    mesh: &MeshGL,
    origin: [f32; 3],
    direction: [f32; 3],
) -> Option<[f32; 3]> {
    if mesh.num_prop < 3 || mesh.vert_properties.is_empty() || mesh.tri_verts.is_empty() {
        return None;
    }
    let stride = mesh.num_prop as usize;
    let n_tri = mesh.tri_verts.len() / 3;
    let mut closest: Option<(f32, [f32; 3])> = None;
    for tri in 0..n_tri {
        let i0 = mesh.tri_verts[tri * 3] as usize;
        let i1 = mesh.tri_verts[tri * 3 + 1] as usize;
        let i2 = mesh.tri_verts[tri * 3 + 2] as usize;
        let p0 = vert(mesh, i0, stride);
        let p1 = vert(mesh, i1, stride);
        let p2 = vert(mesh, i2, stride);
        if let Some(t) = moller_trumbore(origin, direction, p0, p1, p2) {
            if t > 0.0 {
                let take = match closest {
                    Some((best, _)) => t < best,
                    None => true,
                };
                if take {
                    let hit = [
                        origin[0] + direction[0] * t,
                        origin[1] + direction[1] * t,
                        origin[2] + direction[2] * t,
                    ];
                    closest = Some((t, hit));
                }
            }
        }
    }
    closest.map(|(_, p)| p)
}

/// Multi-body picking. For every body in `geometry`, transforms the
/// world-space `(origin, direction)` ray into the body's local space
/// via `body.matrix.inverse()`, ray-tests against the body's mesh, and
/// keeps the closest world-space hit. Returns the hit body's
/// `origin` claim (its source [`NodeId`]) when a body is hit, or
/// `None` when the ray misses every body / the hit body has no claim.
///
/// Each body's mesh lives in local space now that pure-transform ops
/// compose into `Body.matrix` rather than baking — so the ray has to
/// be transformed per body. Matrix inversion is per-body-per-click,
/// which is cheap for the body counts AtomArtist sees (single digits
/// to low hundreds).
///
/// Implements NodeDesigner's "click in 3-D viewport → select origin
/// node" UX. Most-downstream pure-transform op claims the body
/// (Transform overwrites Box's claim), so a click on the rendered
/// `Box → Transform` result selects Transform. Combine / Output
/// preserve per-body claims (they're aggregators), so clicking either
/// of two combined boxes selects the originating Box.
pub fn pick_origin(
    geometry: &Geometry3d,
    ray_origin: [f32; 3],
    ray_direction: [f32; 3],
) -> Option<NodeId> {
    let ray_origin_v = Vec3::from(ray_origin);
    let mut closest: Option<(f32, Option<NodeId>)> = None;
    for body in geometry.iter() {
        // Local-space ray = inverse(body.matrix) · world ray. Skip the
        // body if the matrix is singular.
        let m = Mat4::from_cols_array(&body.matrix);
        let det = m.determinant();
        if !det.is_finite() || det.abs() < 1e-12 {
            continue;
        }
        let inv = m.inverse();
        let local_origin = inv.transform_point3(ray_origin_v);
        let local_dir = inv.transform_vector3(Vec3::from(ray_direction));
        // Normalize the local direction so `raycast_mesh`'s `t` values
        // are local-space distances; world-space distance for closest-
        // hit comparison comes from re-transforming the hit point
        // through `m` and measuring against the camera origin.
        let local_dir_n = match local_dir.try_normalize() {
            Some(d) => d,
            None => continue,
        };
        if let Some(hit_local) = raycast_mesh(
            &body.mesh,
            local_origin.to_array(),
            local_dir_n.to_array(),
        ) {
            let hit_world = m.transform_point3(Vec3::from(hit_local));
            let dist = (hit_world - ray_origin_v).length();
            match closest {
                Some((d, _)) if dist >= d => {}
                _ => closest = Some((dist, body.origin)),
            }
        }
    }
    closest.and_then(|(_, id)| id)
}

/// Pick-target description for a control-gizmo handle. The renderer
/// + gizmo code keep a parallel list of handles per active gizmo;
/// each handle has an integer `id` the gizmo knows how to interpret
/// (e.g. the Z control's 0 = "translate" knob, 1 = "scale" knob).
/// `center` + `half_extent` define an axis-aligned bounding box the
/// click ray tests against — works for both sphere handles (half-
/// extent = radius on each axis) and cube handles (half-extent = half
/// edge length).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GizmoHandle {
    pub id: u32,
    pub center: [f32; 3],
    pub half_extent: [f32; 3],
}

/// Ray-vs-AABB intersection (slab method). Returns the entry distance
/// `t` along the ray (in `direction` units) if the ray intersects the
/// box and `t > 0`; otherwise `None`. Used by handle picking — the
/// AABB is a tight bound for cubes and a loose bound for spheres
/// (the sphere fully inscribes its AABB), good enough for the
/// pixel-scale handles we draw.
fn ray_aabb_intersect(
    origin: [f32; 3],
    direction: [f32; 3],
    aabb_min: [f32; 3],
    aabb_max: [f32; 3],
) -> Option<f32> {
    let mut t_min = f32::NEG_INFINITY;
    let mut t_max = f32::INFINITY;
    for k in 0..3 {
        if direction[k].abs() < 1e-8 {
            // Ray parallel to this slab — must be inside it or miss.
            if origin[k] < aabb_min[k] || origin[k] > aabb_max[k] {
                return None;
            }
        } else {
            let inv_d = 1.0 / direction[k];
            let mut t0 = (aabb_min[k] - origin[k]) * inv_d;
            let mut t1 = (aabb_max[k] - origin[k]) * inv_d;
            if t0 > t1 {
                std::mem::swap(&mut t0, &mut t1);
            }
            if t0 > t_min { t_min = t0; }
            if t1 < t_max { t_max = t1; }
            if t_min > t_max {
                return None;
            }
        }
    }
    if t_max < 0.0 {
        return None;
    }
    // Use the entry distance if the ray starts outside the box;
    // otherwise the exit distance (still positive).
    Some(if t_min > 0.0 { t_min } else { t_max })
}

/// Pick the closest gizmo handle along the ray. Returns the hit
/// handle's `id` (or `None` on miss). Handles are tested in the order
/// supplied; ties resolve by which handle was registered first (so a
/// gizmo can prioritise overlapping handles by ordering them in its
/// active-handles list).
pub fn pick_handle(
    handles: &[GizmoHandle],
    ray_origin: [f32; 3],
    ray_direction: [f32; 3],
) -> Option<u32> {
    let mut closest: Option<(f32, u32)> = None;
    for h in handles {
        let mn = [
            h.center[0] - h.half_extent[0],
            h.center[1] - h.half_extent[1],
            h.center[2] - h.half_extent[2],
        ];
        let mx = [
            h.center[0] + h.half_extent[0],
            h.center[1] + h.half_extent[1],
            h.center[2] + h.half_extent[2],
        ];
        if let Some(t) = ray_aabb_intersect(ray_origin, ray_direction, mn, mx) {
            if t > 0.0 {
                match closest {
                    Some((d, _)) if t >= d => {}
                    _ => closest = Some((t, h.id)),
                }
            }
        }
    }
    closest.map(|(_, id)| id)
}

/// Fallback pivot when [`raycast_mesh`] misses: project the cursor ray
/// onto the plane through the camera's current `center`, perpendicular to
/// the camera's forward vector. Returns the world-space intersection.
///
/// This is the canonical NodeDesigner / MatterCAD "no hit, rotate around
/// the near-pane-aligned plane at the target point" behaviour — keeps the
/// pivot at a sensible scene-anchored depth so the orbit doesn't snap to
/// infinity when the cursor is over empty space.
pub fn project_to_view_plane(
    camera: &OrbitCamera,
    origin: [f32; 3],
    direction: [f32; 3],
) -> [f32; 3] {
    // Forward unit vector from the camera through `center`.
    let (_right, _up, fwd) = camera.basis();
    // Plane: normal = fwd, passes through camera.center. For a point on
    // the plane, fwd · (P - center) = 0.  Substituting P = origin + t*dir:
    //   fwd · (origin - center) + t * (fwd · dir) = 0
    //   t = -(fwd · (origin - center)) / (fwd · dir)
    let to_origin = [
        origin[0] - camera.center[0],
        origin[1] - camera.center[1],
        origin[2] - camera.center[2],
    ];
    let denom = fwd[0] * direction[0] + fwd[1] * direction[1] + fwd[2] * direction[2];
    if denom.abs() < 1e-6 {
        // Ray parallel to plane — fall back to camera.center.
        return camera.center;
    }
    let t = -(fwd[0] * to_origin[0] + fwd[1] * to_origin[1] + fwd[2] * to_origin[2]) / denom;
    [
        origin[0] + direction[0] * t,
        origin[1] + direction[1] * t,
        origin[2] + direction[2] * t,
    ]
}

/// A world-space interaction plane: a point on the plane plus the
/// outward unit normal. Used by `Viewport3dWidget` to anchor pan /
/// rotate / wheel-zoom to a fixed world point under the cursor
/// across a drag — direct port of MatterCAD's `PlaneShape` returned
/// by `SceneInteractionPicking.TryResolveSceneOrFallbackHit`.
///
/// For mesh hits the normal is the screen-centre view direction (so
/// the plane is parallel to the screen at the hit depth); for the
/// empty-scene bed fallback the normal is world +Z and the plane
/// passes through Z=0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HitPlane {
    pub point: [f32; 3],
    pub normal: [f32; 3],
}

impl HitPlane {
    /// Intersect a world-space ray with this plane. Returns `None`
    /// when the ray is parallel to (or nearly so) the plane.
    pub fn ray_intersect(&self, origin: [f32; 3], direction: [f32; 3]) -> Option<[f32; 3]> {
        let denom = self.normal[0] * direction[0]
            + self.normal[1] * direction[1]
            + self.normal[2] * direction[2];
        if denom.abs() < 1e-6 {
            return None;
        }
        let to_point = [
            self.point[0] - origin[0],
            self.point[1] - origin[1],
            self.point[2] - origin[2],
        ];
        let t = (self.normal[0] * to_point[0]
            + self.normal[1] * to_point[1]
            + self.normal[2] * to_point[2])
            / denom;
        Some([
            origin[0] + direction[0] * t,
            origin[1] + direction[1] * t,
            origin[2] + direction[2] * t,
        ])
    }
}

/// Result of [`resolve_pivot_or_fallback`]: the pivot point and the
/// interaction plane to use for subsequent drag math.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PivotResolution {
    pub world_pos: [f32; 3],
    pub plane: HitPlane,
    /// `true` when the resolution came from a real mesh hit;
    /// `false` for the fallback paths (previous pivot's plane, or
    /// empty-scene bed plane). The viewport's circle cursor can use
    /// this to distinguish "pivot on part" from "pivot on bed".
    pub hit_scene: bool,
}

/// Port of MatterCAD's `SceneInteractionPicking.TryResolveSceneOrFallbackHit`.
/// Resolves a pivot point + interaction plane for a mouse-down or
/// wheel-zoom event:
///
/// 1. Raycast the mouse ray against `mesh`. On hit, return the hit
///    point with a plane perpendicular to `screen_center_ray.dir`
///    through it.
/// 2. If `mesh` is `None` (empty scene), intersect the mouse ray
///    with the bed plane (Z = 0, normal = +Z) and return that with
///    the bed plane itself.
/// 3. If `mesh` is `Some` but the ray missed, build a plane
///    perpendicular to `screen_center_ray.dir` through
///    `previous_pivot`, intersect the mouse ray with it, and use
///    that as the fallback pivot. The fallback plane is the same
///    screen-perpendicular plane — pan along it then feels like
///    sliding the scene over the previous interaction depth.
pub fn resolve_pivot_or_fallback(
    mesh: Option<&MeshGL>,
    mouse_ray_origin: [f32; 3],
    mouse_ray_dir: [f32; 3],
    screen_center_ray_dir: [f32; 3],
    previous_pivot: [f32; 3],
) -> PivotResolution {
    // 1. Mesh hit wins.
    if let Some(m) = mesh {
        if let Some(hit) = raycast_mesh(m, mouse_ray_origin, mouse_ray_dir) {
            return PivotResolution {
                world_pos: hit,
                plane: HitPlane {
                    point: hit,
                    normal: screen_center_ray_dir,
                },
                hit_scene: true,
            };
        }
    }
    // 2. Empty scene: bed plane.
    if mesh.is_none() {
        let bed = HitPlane {
            point: [0.0, 0.0, 0.0],
            normal: [0.0, 0.0, 1.0],
        };
        let pos = bed
            .ray_intersect(mouse_ray_origin, mouse_ray_dir)
            .unwrap_or(previous_pivot);
        return PivotResolution {
            world_pos: pos,
            plane: bed,
            hit_scene: false,
        };
    }
    // 3. Non-empty scene, miss: screen-perpendicular plane through
    //    the previous pivot. Intersect the mouse ray with it.
    let plane = HitPlane {
        point: previous_pivot,
        normal: screen_center_ray_dir,
    };
    let pos = plane
        .ray_intersect(mouse_ray_origin, mouse_ray_dir)
        .unwrap_or(previous_pivot);
    PivotResolution {
        world_pos: pos,
        plane,
        hit_scene: false,
    }
}

fn vert(mesh: &MeshGL, i: usize, stride: usize) -> [f32; 3] {
    [
        mesh.vert_properties[i * stride],
        mesh.vert_properties[i * stride + 1],
        mesh.vert_properties[i * stride + 2],
    ]
}

/// Möller-Trumbore ray-triangle intersection. Returns the parametric
/// distance `t` along `direction` at which the ray hits the triangle, or
/// `None` for a miss.  Triangles are *not* back-face culled — selection
/// in a CAD context wants to hit the inside of an open mesh too.
fn moller_trumbore(
    origin: [f32; 3],
    direction: [f32; 3],
    p0: [f32; 3],
    p1: [f32; 3],
    p2: [f32; 3],
) -> Option<f32> {
    const EPS: f32 = 1e-7;
    let edge1 = sub(p1, p0);
    let edge2 = sub(p2, p0);
    let h = cross(direction, edge2);
    let a = dot(edge1, h);
    if a.abs() < EPS {
        return None;
    }
    let f = 1.0 / a;
    let s = sub(origin, p0);
    let u = f * dot(s, h);
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = cross(s, edge1);
    let v = f * dot(direction, q);
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = f * dot(edge2, q);
    if t > EPS { Some(t) } else { None }
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a single triangle as a MeshGL — vertex format `[x, y, z]`
    /// (num_prop = 3).
    fn single_tri(p0: [f32; 3], p1: [f32; 3], p2: [f32; 3]) -> MeshGL {
        let mut props = Vec::new();
        for p in [p0, p1, p2] {
            props.extend_from_slice(&p);
        }
        MeshGL {
            num_prop: 3,
            vert_properties: props,
            tri_verts: vec![0, 1, 2],
            ..Default::default()
        }
    }

    #[test]
    fn ray_hits_triangle_at_origin() {
        // Triangle in z=0 plane.
        let m = single_tri([-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [0.0, 1.0, 0.0]);
        // Ray from (0, 0, 5) pointing toward -Z.
        let hit = raycast_mesh(&m, [0.0, 0.0, 5.0], [0.0, 0.0, -1.0]).unwrap();
        assert!((hit[0]).abs() < 1e-4);
        assert!((hit[1]).abs() < 1e-4);
        assert!((hit[2]).abs() < 1e-4);
    }

    #[test]
    fn ray_misses_triangle_returns_none() {
        let m = single_tri([-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [0.0, 1.0, 0.0]);
        // Ray pointing away from the triangle.
        assert!(raycast_mesh(&m, [0.0, 0.0, -5.0], [0.0, 0.0, -1.0]).is_none());
    }

    #[test]
    fn closest_hit_wins_when_two_triangles_stack() {
        // Two coplanar triangles at different Z; closest should win.
        let mut m = single_tri([-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [0.0, 1.0, 0.0]);
        // Append second triangle at z=2.
        for p in [[-1.0, -1.0, 2.0], [1.0, -1.0, 2.0], [0.0, 1.0, 2.0]] {
            m.vert_properties.extend_from_slice(&p);
        }
        m.tri_verts.extend_from_slice(&[3, 4, 5]);
        // Ray from (0, 0, 5) → -Z. Should hit z=2 first.
        let hit = raycast_mesh(&m, [0.0, 0.0, 5.0], [0.0, 0.0, -1.0]).unwrap();
        assert!((hit[2] - 2.0).abs() < 1e-3);
    }

    #[test]
    fn hit_plane_ray_intersect_returns_point_on_plane() {
        let plane = HitPlane {
            point: [0.0, 0.0, 1.0],
            normal: [0.0, 0.0, 1.0],
        };
        let hit = plane
            .ray_intersect([0.5, -2.0, 5.0], [0.0, 0.4, -1.0])
            .unwrap();
        assert!((hit[2] - 1.0).abs() < 1e-5, "z should land on plane; got {hit:?}");
        // Parallel ray → None.
        assert!(plane.ray_intersect([0.0, 0.0, 5.0], [1.0, 0.0, 0.0]).is_none());
    }

    #[test]
    fn resolve_pivot_falls_back_to_bed_when_no_mesh() {
        // Mouse ray from above pointing down; bed is Z=0.
        let resolved = resolve_pivot_or_fallback(
            None,
            [1.0, 2.0, 5.0],
            [0.0, 0.0, -1.0],
            [0.0, 0.0, -1.0],
            [99.0, 99.0, 99.0],
        );
        assert!((resolved.world_pos[2]).abs() < 1e-5, "bed pivot z = 0");
        assert_eq!(resolved.plane.normal, [0.0, 0.0, 1.0]);
        assert!(!resolved.hit_scene);
    }

    #[test]
    fn resolve_pivot_uses_mesh_hit_when_available() {
        let m = single_tri([-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [0.0, 1.0, 0.0]);
        let resolved = resolve_pivot_or_fallback(
            Some(&m),
            [0.0, 0.0, 5.0],
            [0.0, 0.0, -1.0],
            [0.0, 0.0, -1.0],
            [99.0, 99.0, 99.0],
        );
        assert!(resolved.hit_scene);
        assert!((resolved.world_pos[2]).abs() < 1e-3);
        // Plane is perpendicular to the screen-centre direction.
        assert_eq!(resolved.plane.normal, [0.0, 0.0, -1.0]);
    }

    #[test]
    fn project_to_view_plane_returns_camera_center_when_ray_passes_through_it() {
        let cam = OrbitCamera::default();
        let eye = cam.eye();
        // Ray from eye toward center hits the perpendicular plane at the
        // center exactly — geometric sanity check.
        let dir = {
            let v = [
                cam.center[0] - eye[0],
                cam.center[1] - eye[1],
                cam.center[2] - eye[2],
            ];
            let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            [v[0] / l, v[1] / l, v[2] / l]
        };
        let hit = project_to_view_plane(&cam, eye, dir);
        for k in 0..3 {
            assert!(
                (hit[k] - cam.center[k]).abs() < 1e-3,
                "axis {} mismatch: hit={} center={}",
                k,
                hit[k],
                cam.center[k]
            );
        }
    }

    // ──────────────────────────────────────────────────────────
    // pick_origin (multi-body picking) tests
    // ──────────────────────────────────────────────────────────

    use atomartist_lib::geometry::{Body, Geometry3d};
    use atomartist_lib::graph::node::{identity_matrix, NodeId};
    use std::sync::Arc;

    /// Build a unit triangle Body at world position `tx`. Triangle lies
    /// in the z=0 plane spanning `[-0.5..+0.5, -0.5..+0.5]` in local
    /// coordinates; the body's matrix translates it to `(tx, 0, 0)`.
    fn body_at(tx: f32, origin: Option<NodeId>) -> Body {
        let mesh = single_tri([-0.5, -0.5, 0.0], [0.5, -0.5, 0.0], [0.0, 0.5, 0.0]);
        let mut m = identity_matrix();
        m[12] = tx;
        Body {
            mesh: Arc::new(mesh),
            matrix: m,
            color: [1.0, 1.0, 1.0, 1.0],
            vertex_colors: None,
            origin,
        }
    }

    #[test]
    fn pick_origin_returns_hit_body_origin() {
        // One body translated to x=3; ray aimed at x=3, z=0.
        let geo = Geometry3d::from_body(body_at(3.0, Some(NodeId(7))));
        let picked = pick_origin(&geo, [3.0, 0.0, 5.0], [0.0, 0.0, -1.0]);
        assert_eq!(picked, Some(NodeId(7)));
    }

    #[test]
    fn pick_origin_returns_none_when_ray_misses() {
        let geo = Geometry3d::from_body(body_at(3.0, Some(NodeId(7))));
        // Ray aimed at x=100 — no body there.
        let picked = pick_origin(&geo, [100.0, 0.0, 5.0], [0.0, 0.0, -1.0]);
        assert_eq!(picked, None);
    }

    #[test]
    fn pick_origin_picks_closest_body_when_two_overlap_in_screen_space() {
        // Two bodies, both at x=0 but one at z=1 and the other at z=-2.
        // A ray from (0, 0, 5) pointing down hits the z=1 body first.
        let mut closer = body_at(0.0, Some(NodeId(10)));
        closer.matrix[14] = 1.0;
        let mut farther = body_at(0.0, Some(NodeId(20)));
        farther.matrix[14] = -2.0;
        let geo = Geometry3d::from_bodies(vec![farther, closer]);
        let picked = pick_origin(&geo, [0.0, 0.0, 5.0], [0.0, 0.0, -1.0]);
        assert_eq!(picked, Some(NodeId(10)), "closer body (z=1) should win");
    }

    #[test]
    fn pick_origin_returns_none_when_hit_body_has_no_claim() {
        // A body without an origin claim suppresses selection rather
        // than guessing (matches NodeDesigner: no claim = no selection).
        let geo = Geometry3d::from_body(body_at(0.0, None));
        let picked = pick_origin(&geo, [0.0, 0.0, 5.0], [0.0, 0.0, -1.0]);
        assert_eq!(picked, None);
    }

    // ──────────────────────────────────────────────────────────
    // pick_handle tests
    // ──────────────────────────────────────────────────────────

    #[test]
    fn pick_handle_returns_hit_id_for_centred_ray() {
        let handles = [
            GizmoHandle { id: 7, center: [3.0, 0.0, 0.0], half_extent: [0.5, 0.5, 0.5] },
        ];
        // Ray from (3, 0, 5) pointing -Z lands inside the handle.
        let picked = pick_handle(&handles, [3.0, 0.0, 5.0], [0.0, 0.0, -1.0]);
        assert_eq!(picked, Some(7));
    }

    #[test]
    fn pick_handle_returns_none_on_miss() {
        let handles = [
            GizmoHandle { id: 7, center: [0.0, 0.0, 0.0], half_extent: [0.5, 0.5, 0.5] },
        ];
        let picked = pick_handle(&handles, [10.0, 0.0, 5.0], [0.0, 0.0, -1.0]);
        assert_eq!(picked, None);
    }

    #[test]
    fn pick_handle_picks_closest_when_two_overlap_along_ray() {
        // Two handles along +Z axis — ray from +Z down should hit the
        // upper one first (closer along the ray).
        let handles = [
            GizmoHandle { id: 10, center: [0.0, 0.0, 1.0], half_extent: [0.5, 0.5, 0.5] },
            GizmoHandle { id: 20, center: [0.0, 0.0, -2.0], half_extent: [0.5, 0.5, 0.5] },
        ];
        let picked = pick_handle(&handles, [0.0, 0.0, 5.0], [0.0, 0.0, -1.0]);
        assert_eq!(picked, Some(10), "closer handle (z=1) should win");
    }

    #[test]
    fn pick_handle_ignores_back_facing_hits() {
        // Ray pointing away from the handle should miss even though
        // the box is technically intersected by the infinite line.
        let handles = [
            GizmoHandle { id: 1, center: [0.0, 0.0, -5.0], half_extent: [0.5, 0.5, 0.5] },
        ];
        // Ray at (0,0,5) pointing +Z — handle is at z=-5 (BEHIND the
        // ray's start in +Z direction).
        let picked = pick_handle(&handles, [0.0, 0.0, 5.0], [0.0, 0.0, 1.0]);
        assert_eq!(picked, None);
    }

    #[test]
    fn pick_origin_skips_bodies_with_singular_matrices() {
        // Singular matrix (all zero) — body must be skipped, not crash.
        let mut bad = body_at(0.0, Some(NodeId(99)));
        bad.matrix = [0.0; 16];
        let good = body_at(0.0, Some(NodeId(42)));
        let geo = Geometry3d::from_bodies(vec![bad, good]);
        let picked = pick_origin(&geo, [0.0, 0.0, 5.0], [0.0, 0.0, -1.0]);
        assert_eq!(picked, Some(NodeId(42)), "good body should still match");
    }
}
