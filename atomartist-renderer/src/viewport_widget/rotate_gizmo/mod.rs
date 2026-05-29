//! Rotate gizmo — a port of MatterCAD's `RotateCornerControl`: three
//! per-axis corner handles (X / Y / Z) clustered at the selection's
//! near corner. Each handle is a flat plate (Stage 3: a curved
//! double-arrow icon) that turns accent-coloured on hover and reveals
//! that axis's rotation compass; dragging it spins the body about the
//! world axis through a corner-anchored centre, with angle snapping.
//!
//! This module owns the geometry/layout. The drag state machine lives
//! in `viewport_widget/viewport_widget_interactions.rs`
//! (`CameraDrag::RotateBodyAxis`); the rotation + angle math is shared
//! from `atomartist_lib::graph::node`
//! (`rotate_about_world_axis`, `angle_on_axis_plane`, `normalize_angle`).
//!
//! Colour matches MatterCAD: monochrome — idle = theme text colour,
//! hover = accent — NOT per-axis red/green/blue.

mod compass;
mod corners;
mod handle;

pub use compass::{drag_overlay, ring_and_ticks};
pub use corners::{rotate_axis_layouts, RotateAxisLayout};
pub use handle::plate_handle;

use crate::scene_renderer::gizmo_pass::GizmoTriangleSet;

/// Base id for the three rotate handles fed to `pick_handle`
/// (X = base, Y = base+1, Z = base+2). Distinct from the Z-control
/// sphere handle id (0) so a combined hit-test could tell them apart.
pub const ROTATE_HANDLE_BASE_ID: u32 = 10;

/// Handle id for `axis` (0/1/2) used in `pick_handle`.
pub fn handle_id(axis: u8) -> u32 {
    ROTATE_HANDLE_BASE_ID + axis as u32
}

/// Inverse of [`handle_id`] — recover the axis from a picked handle id.
pub fn axis_from_handle_id(id: u32) -> Option<u8> {
    if (ROTATE_HANDLE_BASE_ID..ROTATE_HANDLE_BASE_ID + 3).contains(&id) {
        Some((id - ROTATE_HANDLE_BASE_ID) as u8)
    } else {
        None
    }
}

/// World unit vector along `axis` — the rotation plane normal.
pub fn axis_unit(axis: u8) -> [f32; 3] {
    let mut v = [0.0; 3];
    v[axis as usize] = 1.0;
    v
}

/// Build the three per-axis handle plates, painting `hovered_axis`
/// accent and the rest idle.
pub fn rotate_handles(
    layouts: &[RotateAxisLayout; 3],
    hovered_axis: Option<u8>,
    idle: [f32; 4],
    accent: [f32; 4],
) -> Vec<GizmoTriangleSet> {
    layouts
        .iter()
        .map(|l| {
            let color = if hovered_axis == Some(l.axis) { accent } else { idle };
            plate_handle(l.control_center, l.axis, l.handle_size, color)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_id_round_trips_for_each_axis() {
        for axis in 0..3u8 {
            assert_eq!(axis_from_handle_id(handle_id(axis)), Some(axis));
        }
        // The Z-control sphere id (0) is not a rotate handle.
        assert_eq!(axis_from_handle_id(0), None);
    }

    #[test]
    fn axis_unit_points_along_the_axis() {
        assert_eq!(axis_unit(0), [1.0, 0.0, 0.0]);
        assert_eq!(axis_unit(1), [0.0, 1.0, 0.0]);
        assert_eq!(axis_unit(2), [0.0, 0.0, 1.0]);
    }

    #[test]
    fn rotate_handles_accents_only_the_hovered_axis() {
        let cam = crate::camera::OrbitCamera::default();
        let layouts = rotate_axis_layouts(([0.0, 0.0, 0.0], [4.0, 4.0, 4.0]), &cam, 720.0);
        let idle = [0.5, 0.5, 0.5, 1.0];
        let accent = [1.0, 0.6, 0.0, 1.0];
        let sets = rotate_handles(&layouts, Some(1), idle, accent);
        assert_eq!(sets.len(), 3);
        assert_eq!(sets[0].color, idle, "X idle");
        assert_eq!(sets[1].color, accent, "Y hovered → accent");
        assert_eq!(sets[2].color, idle, "Z idle");
    }
}
