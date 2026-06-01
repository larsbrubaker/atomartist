//! Rotation compass for the rotate gizmo — the ring + tick marks that
//! appear around an axis on hover/drag, plus (during a drag) the swept
//! wedge, the needle to the current angle, and the eight snap markers.
//!
//! Ports MatterCAD `RotateCornerControl.DrawRotationCompass` /
//! `DrawTickMarks` / `DrawSnappingMarks`. Everything is built in the
//! axis's rotation plane using the same `(u, v)` basis as
//! `handle::plane_basis`, so a plane angle θ maps to
//! `centre + (u·cosθ + v·sinθ)·r` — matching
//! `atomartist_lib::graph::node::angle_on_axis_plane`. Pure geometry
//! builders → unit-testable without a GPU.

use std::f32::consts::TAU;

use crate::camera::OrbitCamera;
use crate::scene_renderer::gizmo_pass::{GizmoLineSet, GizmoTriangleSet};

use super::handle::plane_basis;

/// Ring band width in screen pixels (MatterCAD `RingWidth`).
const RING_WIDTH_PX: f32 = 20.0;
/// Number of radial tick marks around the ring (MatterCAD draws 60).
const TICK_COUNT: usize = 60;
/// Annulus tessellation — segments around the full ring.
const RING_SEGMENTS: usize = 64;
/// Snap points around a full turn (MatterCAD `numSnapPoints`).
const SNAP_POINTS: usize = 8;

/// Screen pixels beyond the handle radius where the live angle readout
/// floats — MatterCAD's `radius + 100 * DeviceScale`.
const READOUT_OFFSET_PX: f32 = 100.0;

/// Inner / outer ring radii (world units) for a handle at `radius`
/// (world centre→control distance), given the per-pixel scale at the
/// rotation centre.
fn ring_radii(radius: f32, upp: f32) -> (f32, f32) {
    let inner = radius + (RING_WIDTH_PX * 0.5) * upp;
    let outer = inner + RING_WIDTH_PX * upp;
    (inner, outer)
}

/// World point at plane-angle `angle`, distance `r` from `center`, in
/// the plane perpendicular to `axis`.
fn ring_pt(center: [f32; 3], axis: u8, r: f32, angle: f32) -> [f32; 3] {
    let (u, v) = plane_basis(axis);
    let (s, c) = angle.sin_cos();
    [
        center[0] + (u[0] * c + v[0] * s) * r,
        center[1] + (u[1] * c + v[1] * s) * r,
        center[2] + (u[2] * c + v[2] * s) * r,
    ]
}

/// World position of the live angle readout — `READOUT_OFFSET_PX`
/// beyond the handle radius, along the drag's *anchor* direction so it
/// stays put while the body spins. Ports MatterCAD's
/// `unitPosition * (radius + 100 * DeviceScale)`. The host projects
/// this to screen and draws [`format_rotation_degrees`] there.
pub fn readout_position(
    center: [f32; 3],
    axis: u8,
    anchor_angle: f32,
    radius: f32,
    upp: f32,
) -> [f32; 3] {
    ring_pt(center, axis, radius + READOUT_OFFSET_PX * upp, anchor_angle)
}

/// Format a rotation (radians) for the on-screen readout, mirroring
/// MatterCAD's `"{0:0.0#}°"` — degrees to one decimal. The angle is the
/// *accumulated* signed rotation, so a >180° (or negative) sweep reads
/// as e.g. `270.0°` / `-270.0°` rather than wrapping.
pub fn format_rotation_degrees(radians: f32) -> String {
    format!("{:.1}°", radians.to_degrees())
}

fn line_set(vertices: Vec<[f32; 3]>, color: [f32; 4]) -> GizmoLineSet {
    GizmoLineSet {
        vertices,
        color,
        matrix: None,
        draw_solid: true,
        draw_overlay: true,
        occluded_alpha: 0.35,
    }
}

fn tri_set(vertices: Vec<[f32; 3]>, color: [f32; 4]) -> GizmoTriangleSet {
    GizmoTriangleSet {
        vertices,
        color,
        matrix: None,
        draw_solid: true,
        draw_overlay: true,
        occluded_alpha: 0.35,
    }
}

/// The ring band (filled translucent annulus) + the 60 radial tick
/// marks. Shown whenever an axis is hovered or being dragged.
/// `accent` tints the band; `text_color` the ticks.
pub fn ring_and_ticks(
    center: [f32; 3],
    axis: u8,
    radius: f32,
    camera: &OrbitCamera,
    viewport_height: f32,
    accent: [f32; 4],
    text_color: [f32; 4],
) -> (GizmoTriangleSet, GizmoLineSet) {
    let upp = camera.world_units_per_pixel_at(center, viewport_height);
    let (inner, outer) = ring_radii(radius, upp);

    // Filled annulus: a quad per segment, emitted double-sided so it
    // reads from either face.
    let mut band = Vec::with_capacity(RING_SEGMENTS * 12);
    for i in 0..RING_SEGMENTS {
        let a0 = (i as f32) / (RING_SEGMENTS as f32) * TAU;
        let a1 = ((i + 1) as f32) / (RING_SEGMENTS as f32) * TAU;
        let pi0 = ring_pt(center, axis, inner, a0);
        let po0 = ring_pt(center, axis, outer, a0);
        let pi1 = ring_pt(center, axis, inner, a1);
        let po1 = ring_pt(center, axis, outer, a1);
        band.extend_from_slice(&[pi0, po0, po1, pi0, po1, pi1]);
        band.extend_from_slice(&[pi0, po1, po0, pi0, pi1, po1]);
    }
    let band_color = [accent[0], accent[1], accent[2], 0.2];

    // 60 radial tick marks from inner to outer radius.
    let mut ticks = Vec::with_capacity(TICK_COUNT * 2);
    for i in 0..TICK_COUNT {
        let a = (i as f32) / (TICK_COUNT as f32) * TAU;
        ticks.push(ring_pt(center, axis, inner, a));
        ticks.push(ring_pt(center, axis, outer, a));
    }

    (tri_set(band, band_color), line_set(ticks, text_color))
}

/// Drag-time feedback: the swept-angle wedge (centre→inner pie sector
/// from the anchor to the current angle), a needle line to the current
/// angle, and the eight snap markers (the active one accent-tinted).
/// `snapped` is the signed rotation from `anchor_angle`.
pub fn drag_overlay(
    center: [f32; 3],
    axis: u8,
    radius: f32,
    anchor_angle: f32,
    snapped: f32,
    camera: &OrbitCamera,
    viewport_height: f32,
    accent: [f32; 4],
    text_color: [f32; 4],
) -> (Vec<GizmoTriangleSet>, Vec<GizmoLineSet>) {
    let upp = camera.world_units_per_pixel_at(center, viewport_height);
    let (inner, outer) = ring_radii(radius, upp);
    let snap_mark_radius = outer + 20.0 * upp;

    let mut tris = Vec::new();
    let mut lines = Vec::new();

    // Swept wedge: a filled fan from the centre out to `inner`, from
    // the anchor angle through the snapped delta. Subdivided so the
    // arc edge stays smooth. Emitted double-sided (front + reversed
    // winding) — the gizmo triangle pass back-face culls, so a
    // single-sided fan vanishes when its plane is viewed from behind
    // (e.g. the horizontal Z-axis compass seen from above).
    if snapped.abs() > 1e-4 {
        let steps = ((snapped.abs() / (TAU / RING_SEGMENTS as f32)).ceil() as usize).max(1);
        let mut wedge = Vec::with_capacity(steps * 6);
        for s in 0..steps {
            let t0 = anchor_angle + snapped * (s as f32 / steps as f32);
            let t1 = anchor_angle + snapped * ((s + 1) as f32 / steps as f32);
            let p0 = ring_pt(center, axis, inner, t0);
            let p1 = ring_pt(center, axis, inner, t1);
            wedge.extend_from_slice(&[center, p0, p1]);
            wedge.extend_from_slice(&[center, p1, p0]);
        }
        tris.push(tri_set(wedge, [accent[0], accent[1], accent[2], 0.35]));
    }

    // Needle: a line from the centre to the outer ring at the current
    // (anchor + snapped) angle.
    let needle_angle = anchor_angle + snapped;
    lines.push(line_set(
        vec![center, ring_pt(center, axis, outer, needle_angle)],
        accent,
    ));

    // Eight snap markers, anchored at `anchor + i·45°`. The marker
    // nearest the snapped angle (when snapped sits on an eighth-turn)
    // is accent-tinted; the rest use the text colour.
    let active = active_snap_index(snapped);
    for i in 0..SNAP_POINTS {
        let a = anchor_angle + (i as f32) * (TAU / SNAP_POINTS as f32);
        let color = if active == Some(i) { accent } else { text_color };
        tris.push(tri_set(snap_marker(center, axis, snap_mark_radius, a, upp), color));
    }

    (tris, lines)
}

/// Which snap marker (0..8) the snapped angle currently sits on, if it
/// landed on an eighth-turn (i.e. a 45° lock is engaged). `None` for
/// fine 1° angles, so no marker is highlighted then.
fn active_snap_index(snapped: f32) -> Option<usize> {
    let eighth = TAU / SNAP_POINTS as f32;
    let k = (snapped / eighth).round();
    if (snapped - k * eighth).abs() < 1e-3 {
        Some((k as i32).rem_euclid(SNAP_POINTS as i32) as usize)
    } else {
        None
    }
}

/// A small triangle marker pointing inward toward the centre, centred
/// at plane-angle `a` on the `snap_mark_radius` circle. Mirrors the
/// arrow shape MatterCAD's `DrawSnappingMarks` builds (a ~15 px tab).
fn snap_marker(center: [f32; 3], axis: u8, snap_mark_radius: f32, a: f32, upp: f32) -> Vec<[f32; 3]> {
    let (u, v) = plane_basis(axis);
    let (s, c) = a.sin_cos();
    // Radial (outward) and tangential unit directions in the plane.
    let radial = [u[0] * c + v[0] * s, u[1] * c + v[1] * s, u[2] * c + v[2] * s];
    let tangent = [-u[0] * s + v[0] * c, -u[1] * s + v[1] * c, -u[2] * s + v[2] * c];
    let tip_in = 10.0 * upp; // points toward centre
    let base_out = 5.0 * upp;
    let half = 7.0 * upp;
    let along = |base: [f32; 3], rad: f32, tan: f32| {
        [
            base[0] + radial[0] * rad + tangent[0] * tan,
            base[1] + radial[1] * rad + tangent[1] * tan,
            base[2] + radial[2] * rad + tangent[2] * tan,
        ]
    };
    let anchor = ring_pt(center, axis, snap_mark_radius, a);
    let tip = along(anchor, -tip_in, 0.0);
    let b1 = along(anchor, base_out, half);
    let b2 = along(anchor, base_out, -half);
    // Double-sided (front + reversed winding): the gizmo triangle pass
    // back-face culls, so a single-winding marker disappears when its
    // plane faces away from the camera — which is exactly the
    // horizontal Z-axis compass viewed from above, the reported
    // "no snap arrows on Z" case.
    vec![tip, b1, b2, tip, b2, b1]
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCENT: [f32; 4] = [1.0, 0.6, 0.0, 1.0];
    const TEXT: [f32; 4] = [0.9, 0.9, 0.9, 1.0];

    fn cam() -> OrbitCamera {
        OrbitCamera::default()
    }

    #[test]
    fn ring_has_band_quads_and_sixty_ticks() {
        let (band, ticks) = ring_and_ticks([0.0, 0.0, 0.0], 2, 5.0, &cam(), 720.0, ACCENT, TEXT);
        // Double-sided annulus: 12 verts per segment.
        assert_eq!(band.vertices.len(), RING_SEGMENTS * 12);
        // 60 ticks × 2 endpoints.
        assert_eq!(ticks.vertices.len(), TICK_COUNT * 2);
        assert_eq!(ticks.color, TEXT);
        // Band is translucent accent.
        assert!((band.color[3] - 0.2).abs() < 1e-6);
    }

    #[test]
    fn ring_vertices_sit_in_the_axis_plane() {
        // Z-axis ring at a centre with z = 3 → every band/tick vertex
        // keeps z = 3.
        let center = [1.0, 2.0, 3.0];
        let (band, ticks) = ring_and_ticks(center, 2, 4.0, &cam(), 720.0, ACCENT, TEXT);
        for vtx in band.vertices.iter().chain(ticks.vertices.iter()) {
            assert!((vtx[2] - 3.0).abs() < 1e-4, "left the Z plane: {vtx:?}");
        }
    }

    #[test]
    fn drag_overlay_emits_wedge_needle_and_eight_markers() {
        let (tris, lines) = drag_overlay(
            [0.0, 0.0, 0.0],
            2,
            5.0,
            0.0,
            std::f32::consts::FRAC_PI_2, // 90°
            &cam(),
            720.0,
            ACCENT,
            TEXT,
        );
        // 1 wedge + 8 snap markers.
        assert_eq!(tris.len(), 1 + SNAP_POINTS);
        // 1 needle line.
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].vertices.len(), 2);
        // Wedge + every snap marker must be double-sided (vertex count
        // a multiple of 6 = two windings per triangle) so they survive
        // back-face culling when the compass plane faces away — the
        // Z-axis "no snap arrows" regression.
        assert_eq!(tris[0].vertices.len() % 6, 0, "wedge must be double-sided");
        for marker in &tris[1..] {
            assert_eq!(marker.vertices.len(), 6, "snap marker must be a double-sided triangle");
        }
    }

    #[test]
    fn zero_rotation_drag_overlay_skips_the_wedge() {
        let (tris, _lines) =
            drag_overlay([0.0, 0.0, 0.0], 2, 5.0, 0.3, 0.0, &cam(), 720.0, ACCENT, TEXT);
        // No wedge at 0° → just the 8 snap markers.
        assert_eq!(tris.len(), SNAP_POINTS);
    }

    #[test]
    fn readout_sits_beyond_the_ring_on_the_anchor_ray() {
        // Z plane (axis 2), anchor along +X (angle 0): the readout must
        // sit radius + READOUT_OFFSET_PX·upp out along +X, in-plane.
        let center = [1.0, 2.0, 3.0];
        let p = readout_position(center, 2, 0.0, 5.0, 1.0);
        assert!((p[0] - (1.0 + 5.0 + READOUT_OFFSET_PX)).abs() < 1e-3, "x = {}", p[0]);
        assert!((p[1] - 2.0).abs() < 1e-3, "y = {}", p[1]);
        assert!((p[2] - 3.0).abs() < 1e-3, "Z-plane readout left its plane: {}", p[2]);
    }

    #[test]
    fn angle_formats_as_degrees_one_decimal() {
        use std::f32::consts::PI;
        assert_eq!(format_rotation_degrees(PI / 4.0), "45.0°");
        assert_eq!(format_rotation_degrees(-PI), "-180.0°");
        assert_eq!(format_rotation_degrees(0.0), "0.0°");
        // Accumulated past a half-turn reads past it, not wrapped.
        assert_eq!(format_rotation_degrees(3.0 * PI / 2.0), "270.0°");
    }

    #[test]
    fn active_snap_index_only_lights_on_eighth_turns() {
        let eighth = TAU / SNAP_POINTS as f32;
        assert_eq!(active_snap_index(0.0), Some(0));
        assert_eq!(active_snap_index(eighth), Some(1));
        assert_eq!(active_snap_index(2.0 * eighth), Some(2));
        // -45° wraps to marker 7.
        assert_eq!(active_snap_index(-eighth), Some(7));
        // A fine 1° angle lights nothing.
        assert_eq!(active_snap_index(3.0_f32.to_radians()), None);
    }
}
