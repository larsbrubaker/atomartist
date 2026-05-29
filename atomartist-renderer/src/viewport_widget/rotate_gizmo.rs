//! Rotate gizmo — a horizontal ring + draggable handle that rotates the
//! selected body about the world vertical (Z) axis through the body's
//! footprint centre. Camera-distance proportional sizing keeps the ring
//! a constant pixel-size at any zoom; theme-aware colour so it reads
//! against light and dark backgrounds.
//!
//! MatterCAD reference: `PartPreviewWindow/View3D/Gui3D/` rotate-corner
//! control. NodeDesigner reference: `rotate-corner-gizmo.js` (813) +
//! `rotate-corner-drag.js` (274). We keep the same idea — a ring you
//! grab and swing — but with a single grab handle on the ring rather
//! than four corner handles, reusing the existing `pick_handle` AABB
//! hit-test and the filled-tri handle pipeline.
//!
//! Output: one `GizmoLineSet` for the ring (a line-list loop) and one
//! `GizmoTriangleSet` for the grab handle. Both pure functions of bbox +
//! camera + viewport — easy to unit-test.

use std::f32::consts::TAU;

use crate::camera::OrbitCamera;
use crate::scene_renderer::gizmo_pass::{sphere_handle, GizmoLineSet, GizmoTriangleSet};

/// One handle id for the rotate gizmo. Distinct from
/// [`super::z_control_gizmo::Z_TRANSLATE_HANDLE_ID`] (0) so a future
/// combined hit-test can tell the two control gizmos apart.
pub const ROTATE_HANDLE_ID: u32 = 1;

/// Grab-handle diameter in screen pixels — a predictable hit target
/// regardless of body size, matching the Z control's handle sizing.
const HANDLE_SIZE_PX: f32 = 12.0;
/// Gap in screen pixels between the body's footprint and the ring, so
/// the ring clears the geometry instead of cutting through it.
const RING_MARGIN_PX: f32 = 28.0;
/// Pixel floor on the ring radius so a tiny body still gets a ring big
/// enough to grab and swing.
const RING_MIN_RADIUS_PX: f32 = 48.0;
/// Ring tessellation. 48 segments reads as a smooth circle at typical
/// viewport scales without burning vertices (each segment is a
/// line-list pair, so this is 96 vertices).
const RING_SEGMENTS: usize = 48;

/// Solved world-space pose of the rotate gizmo for one selected body.
#[derive(Clone, Copy, Debug)]
pub struct RotateGizmoLayout {
    /// World XY of the rotation axis — the footprint centre.
    pub center_xy: [f32; 2],
    /// World Z of the ring + the plane the pointer angle is read on
    /// (the body's mid-height).
    pub plane_z: f32,
    /// Ring radius in world units.
    pub radius: f32,
    /// World position of the grab handle (parked at angle 0, +X side of
    /// the ring). Used by the mouse-down hit-test.
    pub handle_center: [f32; 3],
    /// Grab-handle diameter in world units.
    pub handle_size: f32,
}

/// Solve the rotate gizmo's pose for the supplied world-space AABB. The
/// ring encircles the body at its mid-height, sized so it clears the
/// footprint by a pixel margin and never shrinks below a pixel floor.
/// The caller passes the camera + viewport height so the per-frame
/// world-units-per-pixel factor lands inside the math.
pub fn rotate_layout_for_aabb(
    world_aabb: ([f32; 3], [f32; 3]),
    camera: &OrbitCamera,
    viewport_height: f32,
) -> RotateGizmoLayout {
    let (mn, mx) = world_aabb;
    let cx = (mn[0] + mx[0]) * 0.5;
    let cy = (mn[1] + mx[1]) * 0.5;
    let cz = (mn[2] + mx[2]) * 0.5;
    let upp = camera.world_units_per_pixel_at([cx, cy, cz], viewport_height);
    let half_x = (mx[0] - mn[0]) * 0.5;
    let half_y = (mx[1] - mn[1]) * 0.5;
    // Half-diagonal of the footprint so the ring clears the body's
    // corners, not just its faces.
    let footprint_half_diag = (half_x * half_x + half_y * half_y).sqrt();
    let radius = (footprint_half_diag + RING_MARGIN_PX * upp).max(RING_MIN_RADIUS_PX * upp);
    let handle_size = HANDLE_SIZE_PX * upp;
    RotateGizmoLayout {
        center_xy: [cx, cy],
        plane_z: cz,
        radius,
        handle_center: [cx + radius, cy, cz],
        handle_size,
    }
}

/// Build the rotate gizmo's gizmo sets for the supplied AABB. `color`
/// is the idle colour both the ring and the handle use (the caller
/// passes the theme text colour, matching the Z control).
pub fn rotate_gizmo_for_aabb(
    world_aabb: ([f32; 3], [f32; 3]),
    camera: &OrbitCamera,
    viewport_height: f32,
    color: [f32; 4],
) -> (GizmoLineSet, GizmoTriangleSet) {
    let layout = rotate_layout_for_aabb(world_aabb, camera, viewport_height);
    let ring = build_ring(layout.center_xy, layout.plane_z, layout.radius, color);
    // Sphere handle (round knob) reads as "grab and swing" better than a
    // cube on a ring; radius = half the pixel diameter.
    let handle = sphere_handle(layout.handle_center, (layout.handle_size * 0.5) as f64, color);
    (ring, handle)
}

/// Build the ring as a closed loop of `RING_SEGMENTS` line-list
/// segments in the horizontal plane `z`, centred at `center_xy`.
fn build_ring(center_xy: [f32; 2], z: f32, radius: f32, color: [f32; 4]) -> GizmoLineSet {
    let mut vertices = Vec::with_capacity(RING_SEGMENTS * 2);
    for i in 0..RING_SEGMENTS {
        let a0 = (i as f32) / (RING_SEGMENTS as f32) * TAU;
        let a1 = ((i + 1) as f32) / (RING_SEGMENTS as f32) * TAU;
        vertices.push([center_xy[0] + radius * a0.cos(), center_xy[1] + radius * a0.sin(), z]);
        vertices.push([center_xy[0] + radius * a1.cos(), center_xy[1] + radius * a1.sin(), z]);
    }
    GizmoLineSet {
        vertices,
        color,
        matrix: None,
        draw_solid: true,
        draw_overlay: true,
        // Match the control-gizmo overlay alpha used by the Z control.
        occluded_alpha: 0.35,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

    #[test]
    fn ring_is_a_closed_loop_of_segments() {
        let ring = build_ring([0.0, 0.0], 0.0, 5.0, RED);
        // Line-list: 2 vertices per segment, RING_SEGMENTS segments.
        assert_eq!(ring.vertices.len(), RING_SEGMENTS * 2);
        // First vertex of the loop and last vertex of the loop coincide
        // (the loop closes back on itself).
        let first = ring.vertices.first().unwrap();
        let last = ring.vertices.last().unwrap();
        assert!((first[0] - last[0]).abs() < 1e-4, "loop closes in x");
        assert!((first[1] - last[1]).abs() < 1e-4, "loop closes in y");
    }

    #[test]
    fn ring_vertices_lie_on_the_circle_in_the_plane() {
        let center = [2.0_f32, -1.0];
        let z = 3.0;
        let radius = 7.5;
        let ring = build_ring(center, z, radius, RED);
        for v in &ring.vertices {
            let dx = v[0] - center[0];
            let dy = v[1] - center[1];
            let r = (dx * dx + dy * dy).sqrt();
            assert!((r - radius).abs() < 1e-3, "vertex off the circle: r={r}");
            assert!((v[2] - z).abs() < 1e-6, "vertex off the plane: z={}", v[2]);
        }
    }

    #[test]
    fn layout_centers_on_footprint_and_clears_the_body() {
        let cam = OrbitCamera::default();
        let aabb = ([0.0, 0.0, 0.0], [10.0, 6.0, 4.0]);
        let layout = rotate_layout_for_aabb(aabb, &cam, 720.0);
        assert_eq!(layout.center_xy, [5.0, 3.0]);
        assert_eq!(layout.plane_z, 2.0);
        // Ring must clear the footprint half-diagonal so it doesn't cut
        // through the body's corners.
        let half_diag = (5.0_f32 * 5.0 + 3.0 * 3.0).sqrt();
        assert!(layout.radius > half_diag, "ring inside the body: {} <= {half_diag}", layout.radius);
        // Handle parks on the +X side of the ring at the centre height.
        assert!((layout.handle_center[0] - (layout.center_xy[0] + layout.radius)).abs() < 1e-5);
        assert_eq!(layout.handle_center[1], layout.center_xy[1]);
        assert_eq!(layout.handle_center[2], layout.plane_z);
        assert!(layout.handle_size > 0.0);
    }

    #[test]
    fn tiny_body_still_gets_a_grabbable_ring() {
        // A sub-pixel body must not collapse the ring to nothing — the
        // pixel floor keeps it grabbable.
        let cam = OrbitCamera::default();
        let speck = ([0.0, 0.0, 0.0], [0.001, 0.001, 0.001]);
        let layout = rotate_layout_for_aabb(speck, &cam, 720.0);
        let floor = RING_MIN_RADIUS_PX * cam.world_units_per_pixel_at([0.0, 0.0, 0.0], 720.0);
        assert!((layout.radius - floor).abs() < 1e-3, "radius should hit the pixel floor");
    }

    #[test]
    fn gizmo_threads_caller_color_through_ring_and_handle() {
        let cam = OrbitCamera::default();
        let aabb = ([0.0, 0.0, 0.0], [4.0, 4.0, 4.0]);
        let (ring, handle) = rotate_gizmo_for_aabb(aabb, &cam, 720.0, RED);
        assert_eq!(ring.color, RED);
        assert_eq!(handle.color, RED);
    }
}
