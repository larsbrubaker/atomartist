//! Unit tests for [`super::WgpuSceneRenderer`] — extracted from
//! `scene_renderer/mod.rs` to keep that file under the 800-line
//! guardrail.

use super::WgpuSceneRenderer;

#[test]
fn renderer_is_constructible() {
    let r = WgpuSceneRenderer::new();
    assert!(r.mesh.is_none());
}

/// Bed Z-fight offset nudges the plane toward the camera — when
/// the camera is above `grid_z` the result is below `grid_z`, and
/// vice versa. Magnitude scales with camera distance so the
/// adjustment is invisible at typical zooms but always exceeds
/// depth-buffer precision.
#[test]
fn bed_render_z_nudges_toward_camera() {
    let mut r = WgpuSceneRenderer::new();
    r.grid_z = 0.0;
    // Default camera looks at the origin from radius=300-ish on +Z;
    // exact value isn't important — only the sign of the nudge.
    let eye_z = r.camera.eye()[2];
    let bed_z = r.bed_render_z();
    assert!(bed_z != r.grid_z);
    if eye_z > 0.0 {
        assert!(bed_z < 0.0, "camera above bed -> bed nudged below");
    } else {
        assert!(bed_z > 0.0, "camera below bed -> bed nudged above");
    }
    // 0.004 × distance scaling — the offset for a 100-unit eye
    // should be at least 0.3 (much larger than f32 depth noise).
    assert!(bed_z.abs() > 0.001 * eye_z.abs().max(1.0));
}

#[test]
fn bed_toggle_default_is_on() {
    let r = WgpuSceneRenderer::new();
    assert!(r.draw_grid);
}
