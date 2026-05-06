//! Ray-mesh picking for the 3-D viewport.
//!
//! Provides:
//!   - [`raycast_mesh`] — Möller-Trumbore against every triangle of a
//!     `MeshGL`, returning the closest hit point in world space.
//!   - [`project_to_view_plane`] — fallback used when the cursor misses the
//!     mesh: the world point on the plane through the camera's current
//!     `center` perpendicular to the camera's forward vector. Matches
//!     MatterCAD / NodeDesigner's "rotate around the near-aligned plane at
//!     the target point" behaviour.
//!
//! Both helpers run on the CPU. For the mesh sizes AtomArtist deals with
//! today (a few thousand triangles), brute-force triangle iteration is fine
//! — every UI test point is sub-millisecond. An AABB / BVH accelerator is
//! a future optimisation when scenes grow.

use manifold_rust::types::MeshGL;

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
}
