//! Z control gizmo — vertical arrow + sphere handle anchored above
//! the selected body. Renders only; drag wiring is a follow-up.
//!
//! MatterCAD reference: `PartPreviewWindow/View3D/Gui3D/MoveInZControl.cs`.
//! The visual is a green Z-axis indicator that lets the user drag the
//! selected node's Z translation. This first cut renders the geometry
//! but doesn't yet capture mouse drags — Stage 2 will plumb the drag
//! into a property-write queue.
//!
//! Output: one `GizmoLineSet` for the arrow shaft + arrowhead lines
//! and one `GizmoTriangleSet` for the sphere handle. Both share an
//! optional model matrix so the host can position the gizmo with the
//! same composed transform the body uses.

use crate::scene_renderer::gizmo_pass::{sphere_handle, GizmoLineSet, GizmoTriangleSet};

/// MatterCAD-style Z axis green (`0x44ff44` with a slight desaturation
/// so it reads against both light and dark backgrounds).
pub const Z_CONTROL_COLOR: [f32; 4] = [0.27, 0.85, 0.27, 1.0];

/// One handle id per Z control — single grab target for the
/// translate-Z action. Cube / scale handles can grow id 1 / 2 / etc.
/// when the rest of MoveInZControl ports.
pub const Z_TRANSLATE_HANDLE_ID: u32 = 0;

/// Build a Z control sized for the supplied world-space AABB. Anchored
/// above the AABB's top face; arrow length + handle radius scale from
/// the AABB's diagonal so the gizmo reads at any zoom level. The
/// caller pushes the resulting line / triangle set into the scene
/// state — keeping this function pure of `&mut` arguments avoids
/// borrow-checker friction at call sites.
pub fn z_control_for_aabb(
    world_aabb: ([f32; 3], [f32; 3]),
) -> (GizmoLineSet, GizmoTriangleSet) {
    let (mn, mx) = world_aabb;
    let cx = (mn[0] + mx[0]) * 0.5;
    let cy = (mn[1] + mx[1]) * 0.5;
    let top_z = mx[2];
    let diag = ((mx[0] - mn[0]).powi(2) + (mx[1] - mn[1]).powi(2) + (mx[2] - mn[2]).powi(2)).sqrt();
    let arrow_len = (diag * 0.6).max(1.0);
    let handle_r = (arrow_len * 0.06).max(0.3);
    build_z_control([cx, cy, top_z], arrow_len, handle_r)
}

/// Build the Z control's gizmo sets. `anchor` is the world-space
/// point at the *bottom* of the gizmo (typically the selected body's
/// top face); the arrow extends `+Z` by `arrow_length`. `handle_radius`
/// sizes the sphere at the tip.
///
/// Returns `(lines, sphere)`. The host pushes `lines` into
/// `gizmo_lines` and `sphere` into `gizmo_triangles` each frame; both
/// drop out when selection changes.
pub fn build_z_control(
    anchor: [f32; 3],
    arrow_length: f32,
    handle_radius: f32,
) -> (GizmoLineSet, GizmoTriangleSet) {
    let tip = [anchor[0], anchor[1], anchor[2] + arrow_length];
    // Arrowhead: four short diagonal segments fanning down from the
    // tip, forming a "spike" silhouette. The angled segments make
    // the gizmo direction obvious at any camera angle.
    let head_h = arrow_length * 0.18;
    let head_r = arrow_length * 0.08;
    let head_base_z = tip[2] - head_h;
    let head_legs: [[f32; 3]; 4] = [
        [anchor[0] + head_r, anchor[1], head_base_z],
        [anchor[0] - head_r, anchor[1], head_base_z],
        [anchor[0], anchor[1] + head_r, head_base_z],
        [anchor[0], anchor[1] - head_r, head_base_z],
    ];
    let mut vertices = vec![anchor, tip];
    for leg in head_legs.iter() {
        vertices.push(*leg);
        vertices.push(tip);
    }
    let lines = GizmoLineSet {
        vertices,
        color: Z_CONTROL_COLOR,
        matrix: None,
        draw_solid: true,
        draw_overlay: true,
        // Match NodeDesigner's control-gizmo overlay alpha.
        occluded_alpha: 0.35,
    };
    let sphere_center = [tip[0], tip[1], tip[2] + handle_radius * 0.5];
    let sphere = sphere_handle(sphere_center, handle_radius as f64, Z_CONTROL_COLOR);
    (lines, sphere)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrow_starts_at_anchor_and_extends_up_by_length() {
        let (lines, _) = build_z_control([1.0, 2.0, 3.0], 10.0, 1.0);
        // First two vertices are the shaft: anchor → tip.
        assert_eq!(lines.vertices[0], [1.0, 2.0, 3.0]);
        assert_eq!(lines.vertices[1], [1.0, 2.0, 13.0]);
    }

    #[test]
    fn sphere_handle_sits_above_tip() {
        let (_, sphere) = build_z_control([0.0, 0.0, 0.0], 10.0, 1.0);
        // Sphere centred above tip (z = 10) by handle_radius / 2.
        let mut min_z = f32::INFINITY;
        let mut max_z = f32::NEG_INFINITY;
        for v in &sphere.vertices {
            if v[2] < min_z { min_z = v[2]; }
            if v[2] > max_z { max_z = v[2]; }
        }
        // Sphere of radius 1 centred at z=10.5 → extent [9.5, 11.5].
        assert!((min_z - 9.5).abs() < 1e-3, "min Z expected 9.5, got {min_z}");
        assert!((max_z - 11.5).abs() < 1e-3, "max Z expected 11.5, got {max_z}");
    }

    #[test]
    fn arrow_uses_z_control_green_color() {
        let (lines, sphere) = build_z_control([0.0, 0.0, 0.0], 1.0, 0.1);
        assert_eq!(lines.color, Z_CONTROL_COLOR);
        assert_eq!(sphere.color, Z_CONTROL_COLOR);
    }

    #[test]
    fn arrow_emits_one_shaft_plus_four_arrowhead_legs() {
        let (lines, _) = build_z_control([0.0, 0.0, 0.0], 1.0, 0.1);
        // 1 shaft segment + 4 arrowhead segments × 2 verts each = 10 verts.
        assert_eq!(lines.vertices.len(), 10);
    }
}
