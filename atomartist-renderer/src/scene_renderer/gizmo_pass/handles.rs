//! Builders for the small handle meshes that gizmos use as grab
//! targets — the spheres on the end of a Z-translate arrow, the cubes
//! on a rotate-corner gizmo, the resize boxes on the bounds-edit
//! gizmo. Output as [`GizmoTriangleSet`]s so the renderer's filled-tri
//! gizmo pass draws them with back-face culling + alpha blend (matches
//! MatterCAD's `Object3DControl` solid+overlay variants).
//!
//! The meshes themselves come from the Z-up primitive generators in
//! `atomartist_lib::geometry::primitives` — generated once per build,
//! cheap to flat-pack into triplets for the gizmo pass's TriangleList
//! pipeline. We deliberately don't share vertex buffers across handles
//! (no indexed draw) because:
//!
//!   * Each handle has its own colour / matrix per frame — keeping
//!     them as independent `GizmoTriangleSet`s lets the renderer
//!     uniform-update only the changed handle without rebuilding the
//!     whole gizmo vbuf.
//!   * Vertex counts are tiny (sphere with `seg_u=12, seg_v=6` is
//!     ~150 triangles; cube is 12). Even a dozen handles together is
//!     ~2k triangles → submillisecond per frame.

use atomartist_lib::geometry::{generate_box, generate_cone, generate_sphere};
use manifold_rust::types::MeshGL;

use super::GizmoTriangleSet;

/// Flat-pack a MeshGL's triangles into the [`GizmoTriangleSet::vertices`]
/// triplet layout. Positions only — gizmo handles don't carry normals
/// (the shader doesn't light them).
fn meshgl_to_tri_verts(mesh: &MeshGL) -> Vec<[f32; 3]> {
    let stride = mesh.num_prop as usize;
    let n_tri = mesh.tri_verts.len() / 3;
    let mut verts = Vec::with_capacity(n_tri * 3);
    for t in 0..n_tri {
        for k in 0..3 {
            let i = mesh.tri_verts[t * 3 + k] as usize;
            verts.push([
                mesh.vert_properties[i * stride],
                mesh.vert_properties[i * stride + 1],
                mesh.vert_properties[i * stride + 2],
            ]);
        }
    }
    verts
}

/// Sphere handle centred at `center` with radius `radius`. Used by Z /
/// XY translate gizmos as the grab knob at the tip of an arrow.
/// Default tessellation (`seg_u=12, seg_v=6`) makes a smooth-enough
/// shape at typical viewport scales without burning vertices.
pub fn sphere_handle(center: [f32; 3], radius: f64, color: [f32; 4]) -> GizmoTriangleSet {
    let mesh = generate_sphere(radius, 12, 6);
    let mut vertices = meshgl_to_tri_verts(&mesh);
    for v in vertices.iter_mut() {
        v[0] += center[0];
        v[1] += center[1];
        v[2] += center[2];
    }
    GizmoTriangleSet {
        vertices,
        color,
        matrix: None,
        draw_solid: true,
        draw_overlay: true,
        // Match MatterCAD control-gizmo overlay alpha — same value
        // used by NodeDesigner's `z-control-gizmo.js` and similar.
        occluded_alpha: 0.35,
    }
}

/// Cube handle centred at `center` with edge length `size`. Used by
/// bounds-edit corner handles and rotate-corner gizmos. Always
/// axis-aligned; if a gizmo wants a tilted cube it should set
/// `GizmoTriangleSet.matrix`.
pub fn cube_handle(center: [f32; 3], size: f64, color: [f32; 4]) -> GizmoTriangleSet {
    let mesh = generate_box(size, size, size);
    let mut vertices = meshgl_to_tri_verts(&mesh);
    for v in vertices.iter_mut() {
        v[0] += center[0];
        v[1] += center[1];
        v[2] += center[2];
    }
    GizmoTriangleSet {
        vertices,
        color,
        matrix: None,
        draw_solid: true,
        draw_overlay: true,
        occluded_alpha: 0.35,
    }
}

/// Cone handle centred at `center`, `radius` at the base and `height`
/// tall, apex pointing **+Z**. Used as the Z-translate handle (MatterCAD's
/// `MoveInZControl` arrowhead is a cone, not a box — a box reads as a
/// scale/height handle). `generate_cone` builds it centred on the origin
/// with the apex at `+height/2`, so translating by `center` lands the
/// apex `height/2` above `center`.
pub fn cone_handle(center: [f32; 3], radius: f64, height: f64, color: [f32; 4]) -> GizmoTriangleSet {
    let mesh = generate_cone(radius, height, 16);
    let mut vertices = meshgl_to_tri_verts(&mesh);
    for v in vertices.iter_mut() {
        v[0] += center[0];
        v[1] += center[1];
        v[2] += center[2];
    }
    GizmoTriangleSet {
        vertices,
        color,
        matrix: None,
        draw_solid: true,
        draw_overlay: true,
        occluded_alpha: 0.35,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cone_handle_apex_points_up_from_center() {
        // height 4 (apex +2 / base -2), radius 1, centred at (0,0,10):
        // apex at z=12, base ring at z=8 spanning x∈[-1,1].
        let h = cone_handle([0.0, 0.0, 10.0], 1.0, 4.0, [1.0, 0.0, 0.0, 1.0]);
        let mut mn = [f32::INFINITY; 3];
        let mut mx = [f32::NEG_INFINITY; 3];
        for v in &h.vertices {
            for k in 0..3 {
                if v[k] < mn[k] { mn[k] = v[k]; }
                if v[k] > mx[k] { mx[k] = v[k]; }
            }
        }
        assert!((mx[2] - 12.0).abs() < 1e-3, "apex Z expected 12, got {}", mx[2]);
        assert!((mn[2] - 8.0).abs() < 1e-3, "base Z expected 8, got {}", mn[2]);
        assert!((mx[0] - 1.0).abs() < 1e-3 && (mn[0] + 1.0).abs() < 1e-3, "radius 1 in X");
    }

    #[test]
    fn sphere_handle_translates_to_center() {
        let h = sphere_handle([5.0, -2.0, 7.0], 1.0, [1.0, 0.0, 0.0, 1.0]);
        // Every vertex's distance from (5, -2, 7) should equal radius.
        // The sphere primitive has many vertices duplicated at the
        // poles; check the bounds extent instead.
        let mut mn = [f32::INFINITY; 3];
        let mut mx = [f32::NEG_INFINITY; 3];
        for v in &h.vertices {
            for k in 0..3 {
                if v[k] < mn[k] { mn[k] = v[k]; }
                if v[k] > mx[k] { mx[k] = v[k]; }
            }
        }
        assert!((mn[0] - 4.0).abs() < 1e-3, "min X expected 4.0, got {}", mn[0]);
        assert!((mx[0] - 6.0).abs() < 1e-3, "max X expected 6.0, got {}", mx[0]);
        assert!((mn[1] + 3.0).abs() < 1e-3, "min Y expected -3.0, got {}", mn[1]);
        assert!((mx[1] + 1.0).abs() < 1e-3, "max Y expected -1.0, got {}", mx[1]);
        assert!((mn[2] - 6.0).abs() < 1e-3, "min Z expected 6.0, got {}", mn[2]);
        assert!((mx[2] - 8.0).abs() < 1e-3, "max Z expected 8.0, got {}", mx[2]);
    }

    #[test]
    fn sphere_handle_color_and_flags_match_control_gizmo_defaults() {
        let h = sphere_handle([0.0, 0.0, 0.0], 1.0, [0.2, 0.8, 0.3, 1.0]);
        assert_eq!(h.color, [0.2, 0.8, 0.3, 1.0]);
        assert!(h.draw_solid);
        assert!(h.draw_overlay);
        assert!((h.occluded_alpha - 0.35).abs() < 1e-5);
        assert!(h.matrix.is_none());
    }

    #[test]
    fn cube_handle_produces_12_triangles() {
        let h = cube_handle([0.0, 0.0, 0.0], 1.0, [1.0, 1.0, 1.0, 1.0]);
        // Cube: 6 faces × 2 triangles × 3 verts = 36 vertex entries.
        assert_eq!(h.vertices.len(), 36);
    }

    #[test]
    fn cube_handle_bounds_match_size() {
        let h = cube_handle([10.0, 20.0, -3.0], 2.0, [1.0, 1.0, 1.0, 1.0]);
        let mut mn = [f32::INFINITY; 3];
        let mut mx = [f32::NEG_INFINITY; 3];
        for v in &h.vertices {
            for k in 0..3 {
                if v[k] < mn[k] { mn[k] = v[k]; }
                if v[k] > mx[k] { mx[k] = v[k]; }
            }
        }
        // Size 2 → half-extent 1 around the centre.
        assert!((mn[0] - 9.0).abs() < 1e-5);
        assert!((mx[0] - 11.0).abs() < 1e-5);
        assert!((mn[1] - 19.0).abs() < 1e-5);
        assert!((mx[1] - 21.0).abs() < 1e-5);
        assert!((mn[2] + 4.0).abs() < 1e-5);
        assert!((mx[2] + 2.0).abs() < 1e-5);
    }
}
