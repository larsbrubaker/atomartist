//! Scene fingerprint + cache hit/miss state for the 3-D viewport.
//!
//! The 3-D viewport widget repaints whenever ANY widget in the agg-gui
//! tree invalidates — so dragging a 2-D node-canvas window, opening a
//! menu, or moving the mouse over a 2-D pane all force a `render()`
//! call on [`super::WgpuSceneRenderer`]. Without a cache, every one of
//! those frames would run the full opaque + dual-peel + accumulation
//! chain even though the 3-D output hasn't changed — wasteful, and it
//! prevents progressive accumulation from converging (the sample
//! counter would only advance after genuine 3-D changes if the
//! invalidation path was already aware of them).
//!
//! [`SceneFingerprint`] is a hashable / `Eq` summary of every input the
//! 3-D output depends on. The driver compares the current frame's
//! fingerprint against the previous one and:
//!
//! * If they match and `sample_count >= MAX_SAMPLES`, the driver
//!   skips the entire GPU chain and just re-blits `output_fb` (cheap).
//! * If they match and `sample_count < MAX_SAMPLES`, the driver runs
//!   one more jittered sample and folds it in (progressive refinement).
//! * If they differ, the driver resets `sample_count = 0` and runs the
//!   first sample of a fresh accumulation chain.
//!
//! ## Quantisation
//!
//! Floating-point camera / colour inputs are quantised to integers
//! before hashing. Without quantisation, two semantically-identical
//! frames could produce different fingerprints because of trivial
//! float noise (e.g. matrix products re-evaluated under different
//! optimisation passes). The quantisation step sizes are chosen so:
//!
//! * Camera matrices: `1e-4` — sub-pixel-equivalent on a 1080p
//!   viewport at typical scenes, well below any visible difference.
//! * Colours: `8-bit` per channel packed into a `u32` — the surface
//!   format can't represent finer differences anyway.
//! * World positions (bed Z, outline width): `1e-5` — same reasoning
//!   as the camera matrices.

use std::hash::{Hash, Hasher};

use atomartist_lib::geometry::Body;

use super::gizmo_pass::hash_gizmos;
use super::{RenderStyle, WgpuSceneRenderer};

/// Hash every input the renderer depends on for a body list:
/// mesh pointer (geometry identity), per-vertex colour pointer
/// (vertex-colour swap = different output), per-body matrix (transform
/// change), per-body colour (tint change). The order matters — adding
/// or removing a body, or reordering them, shifts the hash.
pub fn hash_bodies(bodies: &[Body]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for b in bodies {
        let mesh_ptr = b.mesh.vert_properties.as_ptr() as usize as u64;
        let vc_ptr = b
            .vertex_colors
            .as_ref()
            .map(|v| v.as_ptr() as usize as u64)
            .unwrap_or(0);
        mesh_ptr.hash(&mut h);
        vc_ptr.hash(&mut h);
        // Quantise floats so trivial reproject noise doesn't roll
        // the hash — matches the rest of `SceneFingerprint`.
        for &v in &b.matrix {
            quantise(v, FLOAT_Q).hash(&mut h);
        }
        pack_color(b.color).hash(&mut h);
    }
    h.finish()
}

/// All-encompassing fingerprint of the 3-D scene state. Two frames
/// that produce a byte-identical fingerprint are guaranteed to render
/// the same image, so the driver can safely reuse the cached output.
///
/// New fields here must:
///
/// 1. cover EVERY input that affects the rendered 3-D image, and
/// 2. be quantised to integers / discrete enums so fp noise can't
///    mask genuine changes.
///
/// The contrapositive (different fingerprint ⇒ different image) is
/// the cheap direction — it just costs us one extra accumulation
/// chain restart. The expensive direction (same fingerprint ⇒ same
/// image) is what `Eq` and `Hash` need to guarantee.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SceneFingerprint {
    /// Camera view matrix, quantised. Captures eye / target / up
    /// changes from orbit + zoom + pan gestures.
    view_q: [i32; 16],
    /// Camera projection matrix, quantised. Captures fov / aspect /
    /// near/far adjustments.
    proj_q: [i32; 16],
    /// Combined hash of every renderer body's identity: mesh pointer
    /// + per-body matrix + per-body colour + vertex-colour pointer.
    /// When ANY body's geometry / transform / tint changes — or when
    /// a body is added or removed — this changes and the accumulator
    /// resets. `0` when `bodies` is empty.
    bodies_hash: u64,
    /// Pixel size of the offscreen framebuffer. A resize invalidates
    /// every accumulation slot.
    fb_size: (u32, u32),
    /// Discriminant of [`RenderStyle`] — picking a different style
    /// changes which passes draw the user mesh.
    render_style: u8,
    /// Selection outline pass active.
    outline_enabled: bool,
    /// Outline colour, packed to 8 bit per channel.
    outline_color_q: u32,
    /// Outline thickness, quantised.
    outline_width_q: i32,
    /// Mesh base colour, packed to 8 bit per channel.
    base_color_q: u32,
    /// Light 0 direction, quantised. Three components — the shader
    /// renormalises, so we capture pre-normalisation values.
    light_dir_q: [i32; 3],
    /// Light 1 direction, quantised.
    light_dir1_q: [i32; 3],
    /// Per-light diffuse / specular / ambient intensities (light 0)
    /// and diffuse / specular (light 1), plus scene-wide ambient,
    /// material specular tint, packed 8-bit-per-channel into `u32`s
    /// the same way `base_color_q` is. Tweaks via the lighting debug
    /// panel must invalidate accumulation, otherwise the panel won't
    /// preview changes mid-frame.
    light_diffuse0_q: u32,
    light_specular0_q: u32,
    light_ambient0_q: u32,
    light_diffuse1_q: u32,
    light_specular1_q: u32,
    global_ambient_q: u32,
    material_specular_q: u32,
    /// Blinn-Phong shininess exponent, quantised.
    shininess_q: i32,
    /// Bed grid line colour, packed to 8 bit per channel.
    grid_color_q: u32,
    /// Dark-mode flag — the bed renders inverted contact-shadow
    /// brightness when this is set.
    grid_dark: bool,
    /// Bed visibility flag.
    show_bed: bool,
    /// World Z (height) of the bed plane, quantised.
    bed_z_q: i32,
    /// 64-bit hash of the renderer's `gizmo_lines` slice. Any
    /// change to vertex positions, colours, matrices, or per-gizmo
    /// flags rolls this and forces an accumulation restart so the
    /// AA chain redraws against the new gizmo state. See
    /// [`crate::scene_renderer::gizmo_pass::hash_gizmos`] for the
    /// hashing details.
    gizmo_hash: u64,
}

const FLOAT_Q: f32 = 1.0e4;
const FINE_FLOAT_Q: f32 = 1.0e5;

impl SceneFingerprint {
    /// Pure-function summary of every renderer input. Reads only
    /// fields on `r` so the test suite can exercise this without a
    /// GPU device — see [`super::tests`].
    pub fn from_renderer(r: &WgpuSceneRenderer) -> Self {
        let aspect = if r.viewport_size.1 == 0 {
            1.0
        } else {
            r.viewport_size.0 as f32 / r.viewport_size.1 as f32
        };
        let to_rgba = |v: [f32; 3]| [v[0], v[1], v[2], 1.0];
        Self::from_inputs(SceneFingerprintInputs {
            view: r.camera.view_matrix(),
            proj: r.camera.projection_matrix(aspect),
            bodies_hash: hash_bodies(&r.bodies),
            fb_size: r.viewport_size,
            render_style: r.render_style,
            outline_enabled: r.outline_enabled,
            outline_color: r.outline_color,
            outline_width: r.outline_width,
            base_color: r.base_color,
            light_dir: r.light_dir,
            light_dir1: r.light_dir1,
            light_diffuse0: to_rgba(r.light_diffuse0),
            light_specular0: to_rgba(r.light_specular0),
            light_ambient0: to_rgba(r.light_ambient0),
            light_diffuse1: to_rgba(r.light_diffuse1),
            light_specular1: to_rgba(r.light_specular1),
            global_ambient: to_rgba(r.global_ambient),
            material_specular: to_rgba(r.material_specular),
            shininess: r.shininess,
            grid_color: r.grid_line_color,
            grid_dark: r.grid_dark_mode,
            show_bed: r.draw_grid,
            bed_z: r.grid_z,
            gizmo_hash: hash_gizmos(&r.gizmo_lines),
        })
    }

    pub fn from_inputs(i: SceneFingerprintInputs) -> Self {
        Self {
            view_q: quantise_mat4(&i.view, FLOAT_Q),
            proj_q: quantise_mat4(&i.proj, FLOAT_Q),
            bodies_hash: i.bodies_hash,
            fb_size: i.fb_size,
            render_style: render_style_id(i.render_style),
            outline_enabled: i.outline_enabled,
            outline_color_q: pack_color(i.outline_color),
            outline_width_q: quantise(i.outline_width, FINE_FLOAT_Q),
            base_color_q: pack_color(i.base_color),
            light_dir_q: [
                quantise(i.light_dir[0], FLOAT_Q),
                quantise(i.light_dir[1], FLOAT_Q),
                quantise(i.light_dir[2], FLOAT_Q),
            ],
            light_dir1_q: [
                quantise(i.light_dir1[0], FLOAT_Q),
                quantise(i.light_dir1[1], FLOAT_Q),
                quantise(i.light_dir1[2], FLOAT_Q),
            ],
            light_diffuse0_q: pack_color(i.light_diffuse0),
            light_specular0_q: pack_color(i.light_specular0),
            light_ambient0_q: pack_color(i.light_ambient0),
            light_diffuse1_q: pack_color(i.light_diffuse1),
            light_specular1_q: pack_color(i.light_specular1),
            global_ambient_q: pack_color(i.global_ambient),
            material_specular_q: pack_color(i.material_specular),
            shininess_q: quantise(i.shininess, FLOAT_Q),
            grid_color_q: pack_color(i.grid_color),
            grid_dark: i.grid_dark,
            show_bed: i.show_bed,
            bed_z_q: quantise(i.bed_z, FINE_FLOAT_Q),
            gizmo_hash: i.gizmo_hash,
        }
    }
}

/// Test seam for [`SceneFingerprint::from_inputs`] — lets the unit
/// tests build a fingerprint from synthetic data without
/// reaching into [`WgpuSceneRenderer`]'s GPU state.
#[derive(Clone, Copy)]
pub struct SceneFingerprintInputs {
    pub view: [f32; 16],
    pub proj: [f32; 16],
    pub bodies_hash: u64,
    pub fb_size: (u32, u32),
    pub render_style: RenderStyle,
    pub outline_enabled: bool,
    pub outline_color: [f32; 4],
    pub outline_width: f32,
    pub base_color: [f32; 4],
    pub light_dir: [f32; 3],
    pub light_dir1: [f32; 3],
    /// Light 0/1 colour intensities packed as RGBA — A is ignored by
    /// the fingerprint but the field stays `[f32; 4]` so the
    /// `pack_color` helper can be reused.
    pub light_diffuse0: [f32; 4],
    pub light_specular0: [f32; 4],
    pub light_ambient0: [f32; 4],
    pub light_diffuse1: [f32; 4],
    pub light_specular1: [f32; 4],
    pub global_ambient: [f32; 4],
    pub material_specular: [f32; 4],
    pub shininess: f32,
    pub grid_color: [f32; 4],
    pub grid_dark: bool,
    pub show_bed: bool,
    pub bed_z: f32,
    pub gizmo_hash: u64,
}

impl SceneFingerprintInputs {
    /// Synthetic baseline for tests: a deterministic minimal scene
    /// that produces a stable fingerprint regardless of host state.
    /// Callers tweak the field they're testing and assert against
    /// the baseline.
    pub fn baseline_for_tests() -> Self {
        Self {
            view: identity_mat4(),
            proj: identity_mat4(),
            bodies_hash: 0,
            fb_size: (800, 600),
            render_style: RenderStyle::Shaded,
            outline_enabled: false,
            outline_color: [1.0, 0.55, 0.10, 1.0],
            outline_width: 0.05,
            base_color: [0.62, 0.66, 0.78, 1.0],
            light_dir: [-0.577_350_3, -0.577_350_3, 0.577_350_3],
            light_dir1: [0.577_350_3, 0.577_350_3, 0.577_350_3],
            light_diffuse0: [0.7, 0.7, 0.7, 1.0],
            light_specular0: [0.05, 0.05, 0.05, 1.0],
            light_ambient0: [0.0, 0.0, 0.0, 1.0],
            light_diffuse1: [0.5, 0.5, 0.5, 1.0],
            light_specular1: [0.05, 0.05, 0.05, 1.0],
            global_ambient: [0.2, 0.2, 0.2, 1.0],
            material_specular: [1.0, 1.0, 1.0, 1.0],
            shininess: 30.0,
            grid_color: [0.55, 0.58, 0.66, 0.7],
            grid_dark: false,
            show_bed: true,
            bed_z: 0.0,
            gizmo_hash: 0,
        }
    }
}

fn identity_mat4() -> [f32; 16] {
    let mut m = [0.0_f32; 16];
    m[0] = 1.0;
    m[5] = 1.0;
    m[10] = 1.0;
    m[15] = 1.0;
    m
}

#[inline]
fn quantise(v: f32, scale: f32) -> i32 {
    if !v.is_finite() {
        // NaN / inf shouldn't normally land in the fingerprint, but
        // mapping them to a sentinel means a bad input doesn't silently
        // alias to a legitimate value.
        return i32::MIN;
    }
    (v * scale).round() as i32
}

fn quantise_mat4(m: &[f32; 16], scale: f32) -> [i32; 16] {
    let mut out = [0_i32; 16];
    for i in 0..16 {
        out[i] = quantise(m[i], scale);
    }
    out
}

fn pack_color(c: [f32; 4]) -> u32 {
    let to_u8 = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u32;
    (to_u8(c[0]) << 24) | (to_u8(c[1]) << 16) | (to_u8(c[2]) << 8) | to_u8(c[3])
}

fn render_style_id(s: RenderStyle) -> u8 {
    match s {
        RenderStyle::Shaded => 0,
        RenderStyle::Wireframe => 1,
    }
}

/// Stable cache of the last accepted fingerprint + an outcome flag for
/// the current frame. Kept as plain `Cell<Option<...>>` inside
/// [`WgpuSceneRenderer`] for the same lazy-initialisation pattern
/// the bed renderer uses for its `CompositeKey`.
pub fn handle_cache_hit(
    previous: &mut Option<SceneFingerprint>,
    current: SceneFingerprint,
    sample_count: &mut u32,
) -> CacheOutcome {
    match *previous {
        Some(prev) if prev == current => {
            // Fingerprint unchanged — let the driver decide whether to
            // keep accumulating samples or short-circuit on the
            // existing cache.
            CacheOutcome::Hit
        }
        _ => {
            *previous = Some(current);
            *sample_count = 0;
            CacheOutcome::Miss
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheOutcome {
    /// Fingerprint matched — the driver may either skip rendering
    /// (when `sample_count >= MAX_SAMPLES`) or run one more
    /// refinement sample.
    Hit,
    /// Fingerprint changed — `sample_count` was reset to 0; the
    /// driver must run the first sample of a fresh accumulation
    /// chain and clear any stale accumulator state implicitly via
    /// the `weight == 1` blend.
    Miss,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity: the baseline fingerprint round-trips against itself.
    #[test]
    fn fingerprint_is_stable_for_identical_inputs() {
        let a = SceneFingerprint::from_inputs(SceneFingerprintInputs::baseline_for_tests());
        let b = SceneFingerprint::from_inputs(SceneFingerprintInputs::baseline_for_tests());
        assert_eq!(a, b);
    }

    /// A camera change (modifying the view matrix beyond the quantisation
    /// noise threshold) must invalidate the fingerprint — otherwise
    /// rotating the camera wouldn't restart accumulation.
    #[test]
    fn fingerprint_invalidates_on_camera_change() {
        let base = SceneFingerprint::from_inputs(SceneFingerprintInputs::baseline_for_tests());
        let mut tweaked = SceneFingerprintInputs::baseline_for_tests();
        // Translate the view by 1 unit on X — far above the 1e-4
        // quantisation step.
        tweaked.view[12] = 1.0;
        let new_fp = SceneFingerprint::from_inputs(tweaked);
        assert_ne!(base, new_fp);
    }

    /// Mesh swap must invalidate — different `MeshGL` pointers produce
    /// different cache keys.
    #[test]
    fn fingerprint_invalidates_on_mesh_change() {
        let mut a = SceneFingerprintInputs::baseline_for_tests();
        a.bodies_hash = 0xDEAD_BEEF;
        let mut b = SceneFingerprintInputs::baseline_for_tests();
        b.bodies_hash = 0xCAFE_BABE;
        assert_ne!(
            SceneFingerprint::from_inputs(a),
            SceneFingerprint::from_inputs(b),
        );
    }

    /// Multi-body change detection: different body lists must hash
    /// to different values. Two bodies with the same mesh but
    /// different transforms / tints must produce distinct
    /// fingerprints.
    #[test]
    fn hash_bodies_invalidates_on_transform_change() {
        use std::sync::Arc;
        use atomartist_lib::geometry::Body;
        use manifold_rust::types::MeshGL;
        let mesh = Arc::new(MeshGL {
            num_prop: 6,
            vert_properties: vec![0.0; 12],
            tri_verts: vec![0, 1, 0],
            ..Default::default()
        });
        let a = Body::from_mesh(mesh.clone());
        let mut translated = a.matrix;
        translated[12] = 5.0;
        let b = Body::from_mesh(mesh.clone()).with_matrix(translated);
        assert_ne!(hash_bodies(&[a]), hash_bodies(&[b]));
    }

    /// Adding a second body must change the hash.
    #[test]
    fn hash_bodies_grows_with_body_count() {
        use std::sync::Arc;
        use atomartist_lib::geometry::Body;
        use manifold_rust::types::MeshGL;
        let mesh = Arc::new(MeshGL {
            num_prop: 6,
            vert_properties: vec![0.0; 12],
            tri_verts: vec![0, 1, 0],
            ..Default::default()
        });
        let one = vec![Body::from_mesh(mesh.clone())];
        let two = vec![Body::from_mesh(mesh.clone()), Body::from_mesh(mesh)];
        assert_ne!(hash_bodies(&one), hash_bodies(&two));
    }

    /// Confirm the cache does NOT invalidate on inputs that don't reach
    /// `SceneFingerprintInputs` — specifically, "stuff the 3-D viewport
    /// doesn't care about" like 2-D widget state. The fingerprint only
    /// reads fields it declared, so tweaking unrelated inputs in a
    /// hypothetical extended struct would not show up here. This test
    /// instead exercises that re-evaluating the SAME inputs produces
    /// the same fingerprint (the cache-stable direction of the
    /// invariant).
    #[test]
    fn fingerprint_stable_for_unrelated_changes() {
        let mut inputs = SceneFingerprintInputs::baseline_for_tests();
        let base = SceneFingerprint::from_inputs(inputs);
        // Quantisation-noise float perturbation: smaller than 1/FLOAT_Q.
        inputs.bed_z = 1e-7;
        let perturbed = SceneFingerprint::from_inputs(inputs);
        assert_eq!(base, perturbed);
    }

    /// `handle_cache_hit` returns `Miss` on first call (no previous
    /// fingerprint) and resets `sample_count`. Subsequent calls with
    /// the same fingerprint return `Hit` and leave `sample_count`
    /// alone — the driver is responsible for advancing it.
    #[test]
    fn cache_hit_keeps_sample_count() {
        let mut prev: Option<SceneFingerprint> = None;
        let mut sample_count = 0_u32;
        let fp = SceneFingerprint::from_inputs(SceneFingerprintInputs::baseline_for_tests());

        // First call: miss (no previous fingerprint).
        assert_eq!(
            handle_cache_hit(&mut prev, fp, &mut sample_count),
            CacheOutcome::Miss
        );
        assert_eq!(sample_count, 0);

        // Simulate the driver running 5 samples.
        sample_count = 5;
        assert_eq!(
            handle_cache_hit(&mut prev, fp, &mut sample_count),
            CacheOutcome::Hit
        );
        assert_eq!(sample_count, 5, "Hit must NOT reset the sample counter");

        // Saturate.
        sample_count = 16;
        assert_eq!(
            handle_cache_hit(&mut prev, fp, &mut sample_count),
            CacheOutcome::Hit
        );
        assert_eq!(sample_count, 16);

        // Now change the fingerprint — must reset.
        let mut tweaked = SceneFingerprintInputs::baseline_for_tests();
        tweaked.view[12] = 1.0;
        let new_fp = SceneFingerprint::from_inputs(tweaked);
        assert_eq!(
            handle_cache_hit(&mut prev, new_fp, &mut sample_count),
            CacheOutcome::Miss
        );
        assert_eq!(sample_count, 0, "Miss must reset the sample counter");
    }

    /// Different render styles must produce different fingerprints.
    /// Otherwise the cached output for `Shaded` would survive a switch
    /// to `Wireframe` and the user would see stale content.
    #[test]
    fn render_style_change_invalidates() {
        let mut a = SceneFingerprintInputs::baseline_for_tests();
        a.render_style = RenderStyle::Shaded;
        let mut b = SceneFingerprintInputs::baseline_for_tests();
        b.render_style = RenderStyle::Wireframe;
        assert_ne!(
            SceneFingerprint::from_inputs(a),
            SceneFingerprint::from_inputs(b),
        );
    }

    /// A gizmo state change (different `gizmo_hash`) must invalidate
    /// the fingerprint — otherwise resizing the bounds box or
    /// showing/hiding a control gizmo would leave the AA chain
    /// frozen on the previous frame.
    #[test]
    fn gizmo_state_change_invalidates() {
        let mut a = SceneFingerprintInputs::baseline_for_tests();
        a.gizmo_hash = 0;
        let mut b = SceneFingerprintInputs::baseline_for_tests();
        b.gizmo_hash = 0xDEAD_BEEF_CAFE_BABE;
        assert_ne!(
            SceneFingerprint::from_inputs(a),
            SceneFingerprint::from_inputs(b),
        );
    }

    /// Resize must invalidate — the framebuffer + accumulator
    /// allocations get reissued, so any cached output is stale.
    #[test]
    fn resize_invalidates() {
        let mut a = SceneFingerprintInputs::baseline_for_tests();
        a.fb_size = (800, 600);
        let mut b = SceneFingerprintInputs::baseline_for_tests();
        b.fb_size = (1024, 768);
        assert_ne!(
            SceneFingerprint::from_inputs(a),
            SceneFingerprint::from_inputs(b),
        );
    }
}
