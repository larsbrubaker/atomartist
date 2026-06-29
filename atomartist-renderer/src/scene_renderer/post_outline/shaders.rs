//! WGSL shaders for the Blender-style post-process selection outline.
//!
//! Behavioural port of NodeDesigner's
//! [`selection-outline.js`](../../../../../../FDS/NodeDesigner/static/js/node-editor/rendering/selection-outline.js).
//! NodeDesigner runs three passes: a depth-only pass for the full scene,
//! an ID prepass that paints selected meshes flat (=1) into a colour
//! texture (carrying its own depth), and two screen-space edge-detect
//! quad passes (solid + overlay) that sample the ID mask, the
//! selected-mesh depth, and the full-scene depth to decide whether each
//! fragment is on a silhouette edge and whether it's occluded by other
//! geometry.
//!
//! The atomartist port collapses the three passes into two:
//!
//! 1. [`ID_PREPASS_SHADER`] — same idea as NodeDesigner's
//!    `idPrepassFragmentShader`. We rasterize the currently-selected
//!    mesh into an `R8Unorm` mask texture (`1` where covered, `0`
//!    everywhere else) AND mirror its clip-space `z` into an
//!    `R32Float` colour attachment that the edge-detect shader
//!    samples via `textureLoad`. The atomartist opaque pass already
//!    populates a `scene_depth_color` attachment for ALL geometry, so
//!    we reuse THAT as the "full scene depth" — no separate scene
//!    depth-only pass.
//! 2. [`EDGE_DETECT_SHADER`] — replaces NodeDesigner's two
//!    `edgeDetectSolidFragmentShader` + `edgeDetectOverlayFragmentShader`
//!    quads. Same 8-neighbour ID sampling logic; same occlusion test;
//!    a single output alpha-blended over the final scene composite
//!    with `alpha = 1.0` when the outline pixel is visible and
//!    `alpha = OCCLUDED_ALPHA (= 0.35)` when it's behind other
//!    geometry. The output target uses standard `OVER` alpha blending
//!    so the existing scene-composite content shows through where the
//!    shader didn't paint.
//!
//! ## Why one pass, not two
//!
//! NodeDesigner splits the two visibility regimes (visible vs occluded)
//! across two materials because Three.js's standard `transparent` flag
//! controls depth-write + sort order; running both materials lets the
//! occluded-side material avoid corrupting `depth` for the solid side.
//! Atomartist's outline pass runs OVER the already-composited HDR
//! scene buffer (`scene_fb`) with no depth interactions whatsoever, so
//! a single shader that branches on `visible vs occluded` and writes
//! the appropriate alpha is sufficient.

/// Uniform layout consumed by both shaders. `mvp` is needed by the ID
/// prepass vertex shader; `resolution`, `outline_color`,
/// `outline_width`, and `occluded_alpha` are needed by the edge-detect
/// quad. Keeping a single shared layout avoids a second per-frame
/// uniform buffer.
pub const ID_PREPASS_SHADER: &str = r#"
struct U {
    mvp: mat4x4<f32>,
    outline_color: vec4<f32>,
    resolution: vec4<f32>,        // xy = pixel size, zw = pad
    params: vec4<f32>,            // x = outline_width (texels), y = occluded_alpha
};

@group(0) @binding(0) var<uniform> u: U;

struct VOut {
    @builtin(position) clip: vec4<f32>,
};

struct FsOut {
    @location(0) id: vec4<f32>,           // R channel only, R8Unorm target
    @location(1) depth_color: vec4<f32>,  // R channel only, R32Float mirror
};

@vertex
fn vs(@location(0) pos: vec3<f32>, @location(1) _normal: vec3<f32>) -> VOut {
    var o: VOut;
    o.clip = u.mvp * vec4<f32>(pos, 1.0);
    return o;
}

@fragment
fn fs(in: VOut) -> FsOut {
    var out: FsOut;
    // R8Unorm only stores the red channel; the other components are
    // pad for the shader's required vec4<f32> return.
    out.id = vec4<f32>(1.0, 0.0, 0.0, 1.0);
    // R32Float mirror of selected-mesh clip-space z. Sampled by the
    // edge-detect shader to compare against `scene_depth_color`
    // (which the opaque pass populated for all geometry).
    out.depth_color = vec4<f32>(in.clip.z, 0.0, 0.0, 1.0);
    return out;
}
"#;

/// Full-screen post-process edge-detect + outline. Reads:
///
/// * `id_mask` (R8Unorm) — `1` on selected pixels, `0` elsewhere.
/// * `selected_depth` (R32Float) — selected mesh clip-z at each pixel.
/// * `scene_depth` (R32Float) — full-scene clip-z at each pixel.
///
/// Writes a single RGBA colour at `@location(0)` with standard `OVER`
/// alpha blending against the existing composited scene. Where there's
/// no outline (interior or background) the shader discards so the
/// underlying composite shows through unchanged.
pub const EDGE_DETECT_SHADER: &str = r#"
struct U {
    mvp: mat4x4<f32>,
    outline_color: vec4<f32>,
    resolution: vec4<f32>,
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var id_mask: texture_2d<f32>;
@group(0) @binding(2) var selected_depth: texture_2d<f32>;
@group(0) @binding(3) var scene_depth: texture_2d<f32>;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// Standard full-screen triangle covering the [-1, 1] NDC. Same trick
// the dual-peel resolve uses — three vertices, no vbuffer.
@vertex
fn vs(@builtin(vertex_index) idx: u32) -> VOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    var uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );
    var o: VOut;
    o.clip = vec4<f32>(positions[idx], 0.0, 1.0);
    o.uv = uvs[idx];
    return o;
}

// NodeDesigner uses `> 0.5` to test "selected" on a 0/1 mask; `0.5`
// gives one half-step of margin against rounding noise on the
// R8Unorm sampler.
const SELECTED_THRESHOLD: f32 = 0.5;

// Slight epsilon for the occlusion test — verbatim from NodeDesigner
// (`0.0001` in clip-space z) so Z-fighting between selected mesh and
// other geometry doesn't flip occluded vs visible regions.
const OCCLUSION_EPS: f32 = 0.0001;

fn sample_id(pixel: vec2<i32>, dim: vec2<i32>) -> f32 {
    let clamped = clamp(pixel, vec2<i32>(0, 0), dim - vec2<i32>(1, 1));
    return textureLoad(id_mask, clamped, 0).r;
}

fn sample_selected_depth(pixel: vec2<i32>, dim: vec2<i32>) -> f32 {
    let clamped = clamp(pixel, vec2<i32>(0, 0), dim - vec2<i32>(1, 1));
    return textureLoad(selected_depth, clamped, 0).r;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    let dim = vec2<i32>(u.resolution.xy);
    // Centre pixel — `clip.xy` is already in physical pixels.
    let center_px = vec2<i32>(in.clip.xy);
    let offset = i32(round(max(u.params.x, 1.0)));

    let center = sample_id(center_px, dim);
    // Axis-aligned 4-tap.
    let right  = sample_id(center_px + vec2<i32>( offset,  0), dim);
    let left   = sample_id(center_px + vec2<i32>(-offset,  0), dim);
    let up     = sample_id(center_px + vec2<i32>( 0,  offset), dim);
    let down   = sample_id(center_px + vec2<i32>( 0, -offset), dim);
    // Diagonal 4-tap, scaled by 0.707 (≈ 1/sqrt 2) to keep the
    // sample distance from centre roughly uniform across axis vs
    // diagonal — matches NodeDesigner's `offset * 0.707`.
    let diag = i32(round(f32(offset) * 0.7071));
    let tr = sample_id(center_px + vec2<i32>( diag,  diag), dim);
    let tl = sample_id(center_px + vec2<i32>(-diag,  diag), dim);
    let br = sample_id(center_px + vec2<i32>( diag, -diag), dim);
    let bl = sample_id(center_px + vec2<i32>(-diag, -diag), dim);

    let any_selected = (right > SELECTED_THRESHOLD)
        || (left  > SELECTED_THRESHOLD)
        || (up    > SELECTED_THRESHOLD)
        || (down  > SELECTED_THRESHOLD)
        || (tr    > SELECTED_THRESHOLD)
        || (tl    > SELECTED_THRESHOLD)
        || (br    > SELECTED_THRESHOLD)
        || (bl    > SELECTED_THRESHOLD);
    let any_empty = (right < SELECTED_THRESHOLD)
        || (left  < SELECTED_THRESHOLD)
        || (up    < SELECTED_THRESHOLD)
        || (down  < SELECTED_THRESHOLD)
        || (tr    < SELECTED_THRESHOLD)
        || (tl    < SELECTED_THRESHOLD)
        || (br    < SELECTED_THRESHOLD)
        || (bl    < SELECTED_THRESHOLD);

    if (!(any_selected && any_empty)) {
        discard;
    }

    // Occlusion test: take the minimum selected-mesh depth among the
    // sampled neighbours (just like NodeDesigner: "find the depth of
    // the nearest selected pixel"), compare against the full-scene
    // depth at the centre pixel.
    var selected_z = 1.0;
    if (center > SELECTED_THRESHOLD) {
        selected_z = min(selected_z, sample_selected_depth(center_px, dim));
    }
    if (right > SELECTED_THRESHOLD) {
        selected_z = min(selected_z, sample_selected_depth(center_px + vec2<i32>( offset, 0), dim));
    }
    if (left > SELECTED_THRESHOLD) {
        selected_z = min(selected_z, sample_selected_depth(center_px + vec2<i32>(-offset, 0), dim));
    }
    if (up > SELECTED_THRESHOLD) {
        selected_z = min(selected_z, sample_selected_depth(center_px + vec2<i32>(0,  offset), dim));
    }
    if (down > SELECTED_THRESHOLD) {
        selected_z = min(selected_z, sample_selected_depth(center_px + vec2<i32>(0, -offset), dim));
    }

    let scene_z = textureLoad(scene_depth, center_px, 0).r;
    let occluded = selected_z > scene_z + OCCLUSION_EPS;

    var alpha = u.outline_color.a;
    if (occluded) {
        // `params.y` is the occluded-alpha multiplier (NodeDesigner
        // default `0.35`). Standard OVER blending takes the
        // multiplied alpha as final coverage.
        alpha = u.outline_color.a * max(u.params.y, 0.0);
    }
    return vec4<f32>(u.outline_color.rgb, alpha);
}
"#;
