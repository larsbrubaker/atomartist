//! Tests for [`super::accumulation`] — extracted into a sibling file
//! via `#[cfg(test)] #[path = ...] mod tests;` so the production
//! module stays under the 800-line guardrail.

use super::*;

#[test]
fn halton_matches_node_designer() {
    // Reference values produced by running NodeDesigner's
    // `halton(i, 2)` and `halton(i, 3)` in a browser console.
    let h2: [f32; 8] = [0.5, 0.25, 0.75, 0.125, 0.625, 0.375, 0.875, 0.0625];
    let h3: [f32; 8] = [
        1.0 / 3.0,
        2.0 / 3.0,
        1.0 / 9.0,
        4.0 / 9.0,
        7.0 / 9.0,
        2.0 / 9.0,
        5.0 / 9.0,
        8.0 / 9.0,
    ];
    for (i, expected) in h2.iter().enumerate() {
        let got = halton((i as u32) + 1, 2);
        assert!(
            (got - expected).abs() < 1e-6,
            "halton({}, 2): expected {}, got {}",
            i + 1,
            expected,
            got
        );
    }
    for (i, expected) in h3.iter().enumerate() {
        let got = halton((i as u32) + 1, 3);
        assert!(
            (got - expected).abs() < 1e-5,
            "halton({}, 3): expected {}, got {}",
            i + 1,
            expected,
            got
        );
    }
}

#[test]
fn jitter_offset_zero_for_first_sample() {
    assert_eq!(jitter_offset(0), (0.0, 0.0));
}

#[test]
fn jitter_offset_is_centered_in_pixel_for_subsequent_samples() {
    for i in 1..=16 {
        let (x, y) = jitter_offset(i);
        assert!(
            x >= -0.5 && x <= 0.5,
            "jitter x out of [-0.5, 0.5] at i={}: {}",
            i,
            x
        );
        assert!(
            y >= -0.5 && y <= 0.5,
            "jitter y out of [-0.5, 0.5] at i={}: {}",
            i,
            y
        );
    }
}

/// Column-major 4x4 · column vector (length-4).
fn mul_mat4_vec4(m: &[f32; 16], v: [f32; 4]) -> [f32; 4] {
    let mut out = [0.0_f32; 4];
    for r in 0..4 {
        out[r] = m[r] * v[0] + m[4 + r] * v[1] + m[8 + r] * v[2] + m[12 + r] * v[3];
    }
    out
}

/// Project a view-space point through `proj`, return `(ndc.x, ndc.y)`.
fn project_to_ndc_xy(proj: &[f32; 16], view_pos: [f32; 3]) -> (f32, f32) {
    let clip = mul_mat4_vec4(proj, [view_pos[0], view_pos[1], view_pos[2], 1.0]);
    (clip[0] / clip[3], clip[1] / clip[3])
}

#[test]
fn apply_jitter_perspective_only_touches_m02_and_m12() {
    // perspective_rh has m32 = -1, m33 = 0, so the unified jitter
    // formula collapses to "modify m02 / m12 only" — same as
    // NodeDesigner's `applyJitter`.
    let proj_orig =
        glam::Mat4::perspective_rh(60.0_f32.to_radians(), 16.0 / 9.0, 0.1, 1000.0)
            .to_cols_array();
    let mut proj = proj_orig;
    apply_jitter_to_proj(&mut proj, 0.25, -0.125, 1920.0, 1080.0);
    for i in 0..16 {
        if i == 8 || i == 9 {
            assert_ne!(proj[i], proj_orig[i], "m02/m12 should move in perspective");
        } else {
            assert_eq!(
                proj[i], proj_orig[i],
                "perspective jitter modified unexpected slot {}",
                i
            );
        }
    }
}

#[test]
fn apply_jitter_orthographic_only_touches_m03_and_m13() {
    // orthographic_rh has m32 = 0, m33 = 1, so the unified jitter
    // formula collapses to "modify m03 / m13 (translation column)".
    // Touching m02 / m12 in ortho would have produced a
    // depth-dependent offset (the bug this fixes).
    let proj_orig = glam::Mat4::orthographic_rh(-10.0, 10.0, -5.0, 5.0, 0.1, 1000.0)
        .to_cols_array();
    let mut proj = proj_orig;
    apply_jitter_to_proj(&mut proj, 0.25, -0.125, 1920.0, 1080.0);
    for i in 0..16 {
        if i == 12 || i == 13 {
            assert_ne!(proj[i], proj_orig[i], "m03/m13 should move in ortho");
        } else {
            assert_eq!(
                proj[i], proj_orig[i],
                "ortho jitter modified unexpected slot {}",
                i
            );
        }
    }
}

/// Regression: in orthographic projection the jitter must shift
/// every projected point by the **same** NDC delta regardless of
/// view-space depth. The old code shifted m02 / m12 unconditionally;
/// in ortho that multiplied the offset by view-space z, so points
/// at different depths picked up different jitter amounts —
/// breaking SSAA in orthographic.
#[test]
fn ortho_jitter_produces_constant_ndc_shift_regardless_of_depth() {
    let w = 1920.0;
    let h = 1080.0;
    let jx = 0.3;
    let jy = -0.4;
    let proj_orig = glam::Mat4::orthographic_rh(-10.0, 10.0, -5.0, 5.0, 0.1, 1000.0)
        .to_cols_array();
    let mut proj = proj_orig;
    apply_jitter_to_proj(&mut proj, jx, jy, w, h);

    let expected_dx = 2.0 * jx / w;
    let expected_dy = 2.0 * jy / h;

    // Sample several depths spanning the ortho frustum.
    for z in [-1.0_f32, -10.0, -50.0, -200.0, -900.0] {
        let p = [1.234, -0.567, z];
        let (nx0, ny0) = project_to_ndc_xy(&proj_orig, p);
        let (nx1, ny1) = project_to_ndc_xy(&proj, p);
        let got_dx = nx1 - nx0;
        let got_dy = ny1 - ny0;
        assert!(
            (got_dx - expected_dx).abs() < 1e-6,
            "ortho ndc dx at z={}: expected {}, got {}",
            z,
            expected_dx,
            got_dx
        );
        assert!(
            (got_dy - expected_dy).abs() < 1e-6,
            "ortho ndc dy at z={}: expected {}, got {}",
            z,
            expected_dy,
            got_dy
        );
    }
}

/// Sanity: perspective jitter also yields a constant pixel offset
/// across depths (it always did, but lock it in alongside the
/// ortho fix so future refactors can't regress either path).
#[test]
fn perspective_jitter_produces_constant_ndc_shift_regardless_of_depth() {
    let w = 1920.0;
    let h = 1080.0;
    let jx = 0.3;
    let jy = -0.4;
    let proj_orig =
        glam::Mat4::perspective_rh(60.0_f32.to_radians(), w / h, 0.1, 1000.0)
            .to_cols_array();
    let mut proj = proj_orig;
    apply_jitter_to_proj(&mut proj, jx, jy, w, h);

    let mut first: Option<(f32, f32)> = None;
    for z in [-1.0_f32, -10.0, -50.0, -200.0, -900.0] {
        let p = [1.234, -0.567, z];
        let (nx0, ny0) = project_to_ndc_xy(&proj_orig, p);
        let (nx1, ny1) = project_to_ndc_xy(&proj, p);
        let d = (nx1 - nx0, ny1 - ny0);
        match first {
            None => first = Some(d),
            Some(f) => {
                assert!(
                    (d.0 - f.0).abs() < 1e-6 && (d.1 - f.1).abs() < 1e-6,
                    "perspective ndc delta varied with depth: first {:?}, at z={} {:?}",
                    f,
                    z,
                    d
                );
            }
        }
    }
}

#[test]
fn max_samples_matches_node_designer() {
    assert_eq!(MAX_SAMPLES, 16);
}
