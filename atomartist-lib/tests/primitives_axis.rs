//! Structural + orientation tests for the 3-D primitive generators.
//!
//! These are integration tests rather than unit tests living next to
//! `geometry/primitives.rs` because the primitives file is already at
//! the workspace's 800-line guardrail. Splitting tests into a sibling
//! integration test file (alongside `primitives_manifold.rs`) keeps
//! `primitives.rs` editable without immediately tripping the guardrail.
//!
//! Two categories of test live here:
//!
//!   1. **Winding / shape invariants** — face normals point outward on
//!      origin-centered primitives; vertex / triangle counts; smooth
//!      normals are unit-length and point outward from origin.
//!
//!   2. **Z-up axis assertions** — the convention is that "height" runs
//!      along Z (apex at +Z, base ring in the XY plane). Each primitive
//!      gets a test that distinguishes the Z-up implementation from a
//!      Y-up one.

use atomartist_lib::geometry::{
    compute_flat_normals, generate_box, generate_cone, generate_cylinder,
    generate_cylinder_advanced, generate_pyramid, generate_sphere, generate_torus,
    generate_wedge, get_normal, get_pos, num_tris, num_verts,
};
use manifold_rust::types::MeshGL;

// ──────────────────────────────────────────────────────────────────
// Box / cylinder / sphere basic structural tests
// ──────────────────────────────────────────────────────────────────

#[test]
fn box_has_24_verts_and_12_tris() {
    let m = generate_box(1.0, 1.0, 1.0);
    assert_eq!(num_verts(&m), 24);
    assert_eq!(num_tris(&m), 12);
}

#[test]
fn box_normals_are_unit_length_and_axis_aligned() {
    let m = generate_box(1.0, 1.0, 1.0);
    for i in 0..num_verts(&m) {
        let n = get_normal(&m, i);
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        assert!((len - 1.0).abs() < 1e-5, "vert {} normal not unit: {:?}", i, n);
        for k in 0..3 {
            let abs = n[k].abs();
            assert!(abs < 1e-5 || (abs - 1.0).abs() < 1e-5,
                    "non-axis component on box normal: {:?}", n);
        }
    }
}

#[test]
fn box_positions_lie_on_extents() {
    let m = generate_box(2.0, 4.0, 6.0);
    for i in 0..num_verts(&m) {
        let p = get_pos(&m, i);
        assert!((p[0].abs() - 1.0).abs() < 1e-5);
        assert!((p[1].abs() - 2.0).abs() < 1e-5);
        assert!((p[2].abs() - 3.0).abs() < 1e-5);
    }
}

#[test]
fn cylinder_has_correct_vertex_count() {
    let m = generate_cylinder(1.0, 2.0, 8);
    // 8 sides × 4 verts (= 32) + top center + bottom center + 8 top ring
    // + 8 bottom ring = 50.
    assert_eq!(num_verts(&m), 50);
    // 8 sides × 2 tris + 8 top fan + 8 bottom fan = 32.
    assert_eq!(num_tris(&m), 32);
}

#[test]
fn sphere_has_outward_unit_normals() {
    let m = generate_sphere(2.0, 16, 8);
    for i in 0..num_verts(&m) {
        let p = get_pos(&m, i);
        let n = get_normal(&m, i);
        let pl = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
        let nl = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        assert!((nl - 1.0).abs() < 1e-5, "non-unit normal: {:?}", n);
        let dot = (p[0] * n[0] + p[1] * n[1] + p[2] * n[2]) / pl.max(1e-6);
        assert!(dot > 0.999, "normal not outward; dot={}", dot);
    }
}

#[test]
fn box_face_normals_match_after_recompute() {
    let mut m = generate_box(3.0, 3.0, 3.0);
    let original = m.vert_properties.clone();
    compute_flat_normals(&mut m);
    assert_eq!(m.vert_properties.len(), original.len());
    for i in 0..m.vert_properties.len() {
        assert!((m.vert_properties[i] - original[i]).abs() < 1e-5,
                "mismatch at {}: orig={} new={}",
                i, original[i], m.vert_properties[i]);
    }
}

// ──────────────────────────────────────────────────────────────────
// Winding-direction tests (outward face normals)
// ──────────────────────────────────────────────────────────────────

/// Cross-product face normal of triangle `tri` plus the raw 2× area.
fn face_normal(m: &MeshGL, tri: usize) -> ([f32; 3], f32) {
    let i0 = m.tri_verts[tri * 3] as usize;
    let i1 = m.tri_verts[tri * 3 + 1] as usize;
    let i2 = m.tri_verts[tri * 3 + 2] as usize;
    let p0 = get_pos(m, i0);
    let p1 = get_pos(m, i1);
    let p2 = get_pos(m, i2);
    let e1 = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
    let e2 = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
    let n = [
        e1[1] * e2[2] - e1[2] * e2[1],
        e1[2] * e2[0] - e1[0] * e2[2],
        e1[0] * e2[1] - e1[1] * e2[0],
    ];
    let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if l < 1e-12 {
        return ([0.0, 0.0, 0.0], 0.0);
    }
    ([n[0] / l, n[1] / l, n[2] / l], l)
}

fn face_centroid(m: &MeshGL, tri: usize) -> [f32; 3] {
    let i0 = m.tri_verts[tri * 3] as usize;
    let i1 = m.tri_verts[tri * 3 + 1] as usize;
    let i2 = m.tri_verts[tri * 3 + 2] as usize;
    let p0 = get_pos(m, i0);
    let p1 = get_pos(m, i1);
    let p2 = get_pos(m, i2);
    [
        (p0[0] + p1[0] + p2[0]) / 3.0,
        (p0[1] + p1[1] + p2[1]) / 3.0,
        (p0[2] + p1[2] + p2[2]) / 3.0,
    ]
}

/// For origin-centered convex primitives: every face's outward normal
/// must point away from the origin — i.e. `dot(centroid, face_n) > 0`.
fn assert_origin_centered_outward(m: &MeshGL, label: &str) {
    let nt = num_tris(m);
    for t in 0..nt {
        let c = face_centroid(m, t);
        let (n, area2) = face_normal(m, t);
        if area2 < 1e-6 { continue; }
        let d = c[0] * n[0] + c[1] * n[1] + c[2] * n[2];
        assert!(
            d > 1e-4,
            "{label}: tri {t} winding inward — centroid={:?} face_n={:?} dot={}",
            c, n, d
        );
    }
}

#[test]
fn box_faces_wind_outward() {
    assert_origin_centered_outward(&generate_box(2.0, 3.0, 4.0), "box");
}

#[test]
fn cylinder_faces_wind_outward() {
    assert_origin_centered_outward(&generate_cylinder(1.5, 4.0, 16), "cylinder");
}

#[test]
fn cylinder_advanced_full_revolve_winds_outward() {
    let m = generate_cylinder_advanced(2.0, 2.0, 3.0, 12, 0.0, std::f64::consts::TAU);
    assert_origin_centered_outward(&m, "cylinder_advanced full");
}

#[test]
fn cone_faces_wind_outward() {
    assert_origin_centered_outward(&generate_cone(1.0, 2.0, 12), "cone");
}

#[test]
fn pyramid_faces_wind_outward() {
    assert_origin_centered_outward(&generate_pyramid(2.0, 2.0, 2.0), "pyramid");
}

#[test]
fn sphere_faces_wind_outward() {
    assert_origin_centered_outward(&generate_sphere(1.0, 16, 8), "sphere");
}

#[test]
fn torus_faces_wind_outward() {
    // Inner-ring faces point toward the donut hole; their centroid-from-
    // origin dot product is unreliable. Compare cross-product winding
    // against the stored smooth normal instead.
    let m = generate_torus(2.0, 0.5, 16, 8);
    for t in 0..num_tris(&m) {
        let (fn_, _) = face_normal(&m, t);
        let i0 = m.tri_verts[t * 3] as usize;
        let i1 = m.tri_verts[t * 3 + 1] as usize;
        let i2 = m.tri_verts[t * 3 + 2] as usize;
        let n0 = get_normal(&m, i0);
        let n1 = get_normal(&m, i1);
        let n2 = get_normal(&m, i2);
        let avg = [
            (n0[0] + n1[0] + n2[0]) / 3.0,
            (n0[1] + n1[1] + n2[1]) / 3.0,
            (n0[2] + n1[2] + n2[2]) / 3.0,
        ];
        let d = fn_[0] * avg[0] + fn_[1] * avg[1] + fn_[2] * avg[2];
        assert!(d > 0.0, "torus tri {t} cross-normal disagrees stored: dot={}", d);
    }
}

#[test]
fn wedge_faces_wind_outward() {
    let m = generate_wedge(2.0, 2.0, 2.0);
    for t in 0..num_tris(&m) {
        let (fn_, _) = face_normal(&m, t);
        let i0 = m.tri_verts[t * 3] as usize;
        let stored = get_normal(&m, i0);
        let d = fn_[0] * stored[0] + fn_[1] * stored[1] + fn_[2] * stored[2];
        assert!(d > 0.9, "wedge tri {t} cross disagrees stored: dot={} cross={:?} stored={:?}",
                d, fn_, stored);
    }
}

// ──────────────────────────────────────────────────────────────────
// Z-up axis assertions
// ──────────────────────────────────────────────────────────────────

fn z_extent(m: &MeshGL) -> (f32, f32) {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for i in 0..num_verts(m) {
        let z = get_pos(m, i)[2];
        if z < lo { lo = z; }
        if z > hi { hi = z; }
    }
    (lo, hi)
}

fn has_vertex_near(m: &MeshGL, pos: [f32; 3], eps: f32) -> bool {
    for i in 0..num_verts(m) {
        let p = get_pos(m, i);
        let dx = p[0] - pos[0];
        let dy = p[1] - pos[1];
        let dz = p[2] - pos[2];
        if (dx * dx + dy * dy + dz * dz).sqrt() < eps {
            return true;
        }
    }
    false
}

#[test]
fn cone_apex_is_on_plus_z() {
    let m = generate_cone(1.0, 2.0, 16);
    let (lo, hi) = z_extent(&m);
    assert!((hi - 1.0).abs() < 1e-5, "apex Z should be +height/2, got {hi}");
    assert!((lo + 1.0).abs() < 1e-5, "base Z should be -height/2, got {lo}");
    assert!(has_vertex_near(&m, [0.0, 0.0, 1.0], 1e-5),
            "cone must have an apex vertex at (0, 0, +h/2)");
}

#[test]
fn pyramid_apex_is_on_plus_z() {
    let m = generate_pyramid(2.0, 4.0, 6.0);
    let (lo, hi) = z_extent(&m);
    assert!((hi - 2.0).abs() < 1e-5, "apex Z should be +h/2=2.0, got {hi}");
    assert!((lo + 2.0).abs() < 1e-5, "base Z should be -h/2=-2.0, got {lo}");
    assert!(has_vertex_near(&m, [0.0, 0.0, 2.0], 1e-5),
            "pyramid must have an apex vertex at (0, 0, +h/2)");
}

/// Distinguish a Z-up sphere from a Y-up one by counting vertices on
/// each axis: the pole row collapses many longitude verts into one
/// point, so the pole axis has dozens of duplicates while the equator
/// crossings on other axes only hit once or twice.
#[test]
fn sphere_poles_are_on_z_axis() {
    let m = generate_sphere(3.0, 16, 8);
    let mut z_pole_hits = 0;
    let mut y_pole_hits = 0;
    for i in 0..num_verts(&m) {
        let p = get_pos(&m, i);
        if p[0].abs() < 1e-4 && p[1].abs() < 1e-4 && (p[2].abs() - 3.0).abs() < 1e-4 {
            z_pole_hits += 1;
        }
        if p[0].abs() < 1e-4 && p[2].abs() < 1e-4 && (p[1].abs() - 3.0).abs() < 1e-4 {
            y_pole_hits += 1;
        }
    }
    assert!(z_pole_hits > 10,
            "expected many Z-axis pole vertices, found {z_pole_hits}");
    assert!(y_pole_hits <= 2,
            "Y-axis should only carry equator crossings (≤2), found {y_pole_hits}");
}

#[test]
fn torus_ring_lies_in_xy_plane() {
    let major = 5.0_f64;
    let minor = 1.5_f64;
    let m = generate_torus(major, minor, 16, 8);
    let (lo_z, hi_z) = z_extent(&m);
    assert!((hi_z - minor as f32).abs() < 1e-4,
            "max Z should equal minor_r ({minor}), got {hi_z}");
    assert!((lo_z + minor as f32).abs() < 1e-4,
            "min Z should equal -minor_r, got {lo_z}");
    let mut xy_max = 0.0_f32;
    for i in 0..num_verts(&m) {
        let p = get_pos(&m, i);
        xy_max = xy_max.max(p[0].abs()).max(p[1].abs());
    }
    let expect = (major + minor) as f32;
    assert!((xy_max - expect).abs() < 1e-3,
            "max XY-radius should equal major+minor ({expect}), got {xy_max}");
}

#[test]
fn wedge_slope_rises_along_z() {
    let m = generate_wedge(4.0, 6.0, 2.0);
    let (lo_z, hi_z) = z_extent(&m);
    assert!((hi_z - 3.0).abs() < 1e-5, "top Z should be +h/2=3.0, got {hi_z}");
    assert!((lo_z + 3.0).abs() < 1e-5, "bottom Z should be -h/2=-3.0, got {lo_z}");
    for i in 0..num_verts(&m) {
        let p = get_pos(&m, i);
        if (p[2] - 3.0).abs() < 1e-5 {
            assert!((p[0] + 2.0).abs() < 1e-5,
                    "top edge of wedge must sit at x=-w/2=-2.0, got vertex {:?}", p);
        }
    }
    assert!(has_vertex_near(&m, [2.0, 1.0, -3.0], 1e-5)
            || has_vertex_near(&m, [2.0, -1.0, -3.0], 1e-5),
            "wedge must have a bottom-front edge at x=+w/2, z=-h/2");
}
