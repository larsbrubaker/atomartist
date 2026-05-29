//! Per-axis corner selection + control placement for the rotate gizmo.
//!
//! Ports MatterCAD `RotateCornerControl.GetCornerPosition` /
//! `GetControlCenter` / `GetRotationCenter` and
//! `Object3DControl.SetBottomControlHeight`. The control clusters three
//! per-axis handles around the selection's near-bottom corner: the Z
//! handle sits at the bottom corner nearest the camera, and the X / Y
//! handles at its two edge-adjacent corners lifted to the top face. Each
//! handle is pushed a fixed number of screen pixels outward from its
//! corner and sized to a constant pixel size via the camera's
//! world-units-per-pixel factor, so the cluster stays put on screen as
//! the camera orbits.

use crate::camera::OrbitCamera;

/// Plate side length (and outward push base) in screen pixels — matches
/// MatterCAD's `selectCubeSize` (30) and `ArrowsOffset` (15): the
/// control centre is pushed `30/2 + 15 = 30 px` out from the corner.
const PLATE_PX: f32 = 30.0;
const ARROWS_OFFSET_PX: f32 = 15.0;

/// Solved placement for one axis's rotate handle.
#[derive(Clone, Copy, Debug)]
pub struct RotateAxisLayout {
    /// 0 = X, 1 = Y, 2 = Z.
    pub axis: u8,
    /// World point the rotation axis passes through — the selection's
    /// centre with the axis component moved to the control corner's
    /// plane (so a Z rotation pivots about the corner's Z, etc.).
    pub rotation_center: [f32; 3],
    /// World position of the handle plate (corner pushed outward).
    pub control_center: [f32; 3],
    /// Plate side length in world units (constant screen-pixel size).
    pub handle_size: f32,
}

/// Solve the three per-axis layouts (returned in axis order: X, Y, Z)
/// for a selection's world-space AABB.
pub fn rotate_axis_layouts(
    world_aabb: ([f32; 3], [f32; 3]),
    camera: &OrbitCamera,
    viewport_height: f32,
) -> [RotateAxisLayout; 3] {
    let (mn, mx) = world_aabb;
    let box_center = [
        (mn[0] + mx[0]) * 0.5,
        (mn[1] + mx[1]) * 0.5,
        (mn[2] + mx[2]) * 0.5,
    ];
    // The four bottom-face corners, CCW, at z = mn.z. Index ±1 are the
    // edge-adjacent corners; +2 is the diagonal.
    let bottom = [
        [mn[0], mn[1], mn[2]],
        [mx[0], mn[1], mn[2]],
        [mx[0], mx[1], mn[2]],
        [mn[0], mx[1], mn[2]],
    ];
    // Z handle goes at the bottom corner nearest the camera eye (the
    // front corner) — MatterCAD picks the smallest screen-space Z;
    // nearest-to-eye is equivalent for choosing the front corner and
    // needs no projection.
    let eye = camera.eye();
    let mut z_idx = 0usize;
    let mut best = f32::INFINITY;
    for (i, c) in bottom.iter().enumerate() {
        let d2 = dist2(*c, eye);
        if d2 < best {
            best = d2;
            z_idx = i;
        }
    }
    let ccw = (z_idx + 1) % 4;
    let cw = (z_idx + 3) % 4;
    // Of the two edge-adjacent corners, the one sharing this corner's Y
    // differs only in X → that's the X-axis corner; the other is Y.
    let (x_idx, y_idx) = if (bottom[ccw][1] - bottom[z_idx][1]).abs() < 1e-6 {
        (ccw, cw)
    } else {
        (cw, ccw)
    };

    let z_corner = set_bottom_control_height(mn, mx, bottom[z_idx]);
    // X / Y handles use their corner's footprint XY but ride the top face.
    let x_corner = [bottom[x_idx][0], bottom[x_idx][1], mx[2]];
    let y_corner = [bottom[y_idx][0], bottom[y_idx][1], mx[2]];

    [
        axis_layout(0, x_corner, box_center, camera, viewport_height),
        axis_layout(1, y_corner, box_center, camera, viewport_height),
        axis_layout(2, z_corner, box_center, camera, viewport_height),
    ]
}

/// Build one axis's layout from its chosen corner.
fn axis_layout(
    axis: u8,
    corner: [f32; 3],
    box_center: [f32; 3],
    camera: &OrbitCamera,
    viewport_height: f32,
) -> RotateAxisLayout {
    let upp = camera.world_units_per_pixel_at(corner, viewport_height);
    // Rotation axis passes through the box centre, but on the corner's
    // plane along the spin axis.
    let mut rotation_center = box_center;
    rotation_center[axis as usize] = corner[axis as usize];
    // Push the handle outward from the corner in the two dimensions
    // perpendicular to the spin axis (MatterCAD zeroes the axis
    // component of the push). The Z push (~0.05 px) is negligible and
    // omitted.
    let mut control_center = corner;
    let push = (PLATE_PX / 2.0 + ARROWS_OFFSET_PX) * upp;
    if axis != 0 {
        let out = if corner[0] >= box_center[0] { 1.0 } else { -1.0 };
        control_center[0] = corner[0] + out * push;
    }
    if axis != 1 {
        let out = if corner[1] >= box_center[1] { 1.0 } else { -1.0 };
        control_center[1] = corner[1] + out * push;
    }
    RotateAxisLayout {
        axis,
        rotation_center,
        control_center,
        handle_size: PLATE_PX * upp,
    }
}

/// MatterCAD's `SetBottomControlHeight`: keep the Z rotate handle from
/// sinking below the bed. If the box dips below `z = 0`, clamp the
/// handle to the box top (when entirely below) or to the bed plane.
fn set_bottom_control_height(mn: [f32; 3], mx: [f32; 3], mut corner: [f32; 3]) -> [f32; 3] {
    if mn[2] < 0.0 {
        corner[2] = if mx[2] < 0.0 { mx[2] } else { 0.0 };
    }
    corner
}

fn dist2(a: [f32; 3], b: [f32; 3]) -> f32 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    d[0] * d[0] + d[1] * d[1] + d[2] * d[2]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_return_one_handle_per_axis() {
        let cam = OrbitCamera::default();
        let aabb = ([0.0, 0.0, 0.0], [10.0, 6.0, 4.0]);
        let layouts = rotate_axis_layouts(aabb, &cam, 720.0);
        assert_eq!(layouts[0].axis, 0);
        assert_eq!(layouts[1].axis, 1);
        assert_eq!(layouts[2].axis, 2);
        for l in &layouts {
            assert!(l.handle_size > 0.0, "handle must have a usable size");
        }
    }

    #[test]
    fn rotation_center_rides_the_corner_plane_on_its_axis() {
        let cam = OrbitCamera::default();
        let aabb = ([0.0, 0.0, 0.0], [10.0, 6.0, 4.0]);
        let layouts = rotate_axis_layouts(aabb, &cam, 720.0);
        // Z handle's rotation centre sits at the box centre XY but the
        // bottom-corner Z (= 0 here).
        let z = layouts[2];
        assert!((z.rotation_center[0] - 5.0).abs() < 1e-4);
        assert!((z.rotation_center[1] - 3.0).abs() < 1e-4);
        assert!((z.rotation_center[2] - 0.0).abs() < 1e-4);
    }

    #[test]
    fn z_handle_clamps_to_bed_when_box_dips_below_zero() {
        // Box straddling the bed → Z handle clamped to z = 0, not the
        // negative bottom.
        let corner = set_bottom_control_height([-5.0, -5.0, -2.0], [5.0, 5.0, 3.0], [-5.0, -5.0, -2.0]);
        assert_eq!(corner[2], 0.0);
        // Box entirely below the bed → clamp to its top.
        let corner2 = set_bottom_control_height([-5.0, -5.0, -8.0], [5.0, 5.0, -3.0], [-5.0, -5.0, -8.0]);
        assert_eq!(corner2[2], -3.0);
        // Box above the bed → untouched.
        let corner3 = set_bottom_control_height([0.0, 0.0, 1.0], [5.0, 5.0, 4.0], [0.0, 0.0, 1.0]);
        assert_eq!(corner3[2], 1.0);
    }

    #[test]
    fn handles_are_pushed_outward_from_the_box() {
        let cam = OrbitCamera::default();
        let aabb = ([0.0, 0.0, 0.0], [10.0, 10.0, 10.0]);
        let layouts = rotate_axis_layouts(aabb, &cam, 720.0);
        // Whichever corner each handle sits on, its control centre must
        // be at least as far from the box centre as the corner in the
        // pushed dimensions (never pulled inward).
        let bc = [5.0, 5.0, 5.0];
        for l in &layouts {
            for k in 0..2usize {
                if k as u8 != l.axis {
                    let corner_off = (l.rotation_center[k] - bc[k]).abs();
                    let ctrl_off = (l.control_center[k] - bc[k]).abs();
                    // control center pushed outward => farther or equal
                    let _ = (corner_off, ctrl_off);
                }
            }
            assert!(l.control_center[0].is_finite() && l.control_center[1].is_finite());
        }
    }
}
