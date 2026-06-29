//! WGSL ports of MatterCAD's dual depth-peeling pixel shaders
//! ([`NodeDesignerScene.hlsl`](../../../../../../MatterCAD/Submodules/agg-sharp/VorticeD3D/Shaders/NodeDesignerScene.hlsl)).
//!
//! Three shader programs make up the chain:
//!
//! 1. [`DUAL_DEPTH_INIT_SHADER`] — runs once per frame, before the first
//!    peel iteration. Each transparent fragment writes `(-z, z)` into
//!    the dual-depth `Rgba32Float` target (only the RG channels are
//!    used); paired with a `Max`-blended pipeline, the per-pixel result
//!    is the [`-nearestZ`, `farthestZ`] slab across all transparent
//!    fragments. Discards fragments lying behind the populated
//!    opaque-pass depth so peeling never wastes layers on hidden
//!    geometry. WGSL port of `DualDepthInitPS`.
//!
//! 2. [`DUAL_PEEL_COLOR_SHADER`] — runs once per peel iteration. Reads
//!    the previous slab from the source dual-depth texture, writes the
//!    expanded slab to the destination dual-depth texture, and routes
//!    fragments matching the current `frontZ` to the **front** MRT
//!    slot and fragments matching `backZ` to the **back** MRT slot.
//!    Discards anything strictly inside the slab (it belongs to a
//!    later iteration) or strictly outside (already peeled). WGSL port
//!    of `SceneColorDualPeelPS`.
//!
//! 3. [`DUAL_PEEL_RESOLVE_SHADER`] — runs once after the last peel
//!    iteration. Full-screen quad that combines the opaque-pass color,
//!    the front accumulator (premultiplied) and the back accumulator
//!    (straight) using [`MatterCAD's `ResolveForComposition`
//!    formula`](../../../../../../MatterCAD/Submodules/agg-sharp/VorticeD3D/DualDepthPeelingMath.cs).
//!    The output goes straight into the HDR scene composite that the
//!    renderer box-downsamples to the widget rect.
//!
//! The shader-side discard tolerance (`PEEL_BIAS = 1e-5`) is identical
//! to MatterCAD's `DepthPeelBias`, so iteration-count behaviour is
//! frame-for-frame equivalent.

/// Uniform layout shared by the init + peel passes. `mvp` projects
/// per-vertex positions; `resolution` lets the fragment shader recover
/// screen-space UVs for sampling the opaque-depth and source-dual-depth
/// textures. Color comes from a separate uniform field so the same
/// shader covers both the no-shading init pass and the lit peel pass.
pub const DUAL_DEPTH_INIT_SHADER: &str = r#"
struct U {
    mvp: mat4x4<f32>,
    resolution: vec4<f32>, // xy = pixel size, zw = pad
};

// Per-body uniform — same layout as the opaque shader's `B` block,
// dispatched via the dynamic-offset bind group at group(1).
struct B {
    model: mat4x4<f32>,
    color: vec4<f32>,
    flags: vec4<u32>,
};

@group(0) @binding(0) var<uniform> u: U;
// `opaque_depth_color` is the R32Float mirror of the opaque depth
// attachment populated by the scene / bed pipelines. We sample it
// via `textureLoad` (no sampler needed) because Naga's WebGL2
// backend can't load directly from `texture_depth_2d` — see the
// long-form rationale in `scene_renderer::opaque_shaders`.
@group(0) @binding(1) var opaque_depth_color: texture_2d<f32>;
@group(1) @binding(0) var<uniform> b: B;

struct VOut {
    @builtin(position) clip: vec4<f32>,
};

@vertex
fn vs(
    @location(0) pos: vec3<f32>,
    @location(1) _normal: vec3<f32>,
    // Init shader doesn't read the per-vertex colour, but the
    // attribute must be declared so a single vertex layout serves
    // every per-body pipeline (opaque + depth-only + init + peel +
    // shadow). The argument is bound to the slot-1 buffer the caller
    // provided; ignored here.
    @location(2) _v_color: vec4<f32>,
) -> VOut {
    var o: VOut;
    // Apply the per-body model BEFORE the cached `mvp`. mvp here is
    // proj*view — the host doesn't fold model into it because that
    // would require N mvps per frame (one per body) on top of the
    // dynamic-offset uniform we already use.
    o.clip = u.mvp * b.model * vec4<f32>(pos, 1.0);
    return o;
}

// Discard threshold for "this fragment's z is below the opaque-pass z"
// and "this fragment's z matches the slab boundary". Sized for the
// `Rgba16Float` dual-depth precision floor (~5e-4 in [0,1] NDC) so
// half-float rounding can't accidentally classify a near-slab fragment
// as inside the slab.
const PEEL_BIAS: f32 = 1e-3;

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(clamp(in.clip.xy, vec2<f32>(0.0), u.resolution.xy - vec2<f32>(1.0)));
    let opaque_z = textureLoad(opaque_depth_color, pixel, 0).r;
    if (opaque_z < in.clip.z - PEEL_BIAS) {
        discard;
    }
    // Write (-z, z, 0, 0). The pipeline uses Max blending on RG so the
    // accumulated value is (-min(z), max(z)) across all fragments at
    // this pixel.
    return vec4<f32>(-in.clip.z, in.clip.z, 0.0, 0.0);
}
"#;

/// Uniform layout for the dual-peel colour pass. Layout matches the
/// opaque scene shader's `U` block field-for-field (see
/// [`crate::scene_renderer::opaque_pass::Uniforms`]) so both shaders
/// can share the same Rust struct — cross-layer colour consistency
/// is preserved because a translucent fragment and the opaque scene
/// behind it run identical Blinn-Phong math.
///
/// Port of NodeDesigner's `depth-peeling.js::generateFragmentShader`:
/// view-space lighting via `proj`/`view` split, dual camera-fixed
/// directional lights, configurable shininess, sRGB-encoded base
/// colour.
pub const DUAL_PEEL_COLOR_SHADER: &str = r#"
struct U {
    proj: mat4x4<f32>,
    view: mat4x4<f32>,
    light_dir0: vec4<f32>,
    light_dir1: vec4<f32>,
    light_diffuse0: vec4<f32>,
    light_specular0: vec4<f32>,
    light_ambient0: vec4<f32>,
    light_diffuse1: vec4<f32>,
    light_specular1: vec4<f32>,
    global_ambient: vec4<f32>,
    material_specular: vec4<f32>,
    base_color: vec4<f32>,    // fallback only — body colour preferred
    params: vec4<f32>,        // x = shininess
    resolution: vec4<f32>,    // xy = pixel size
};

struct B {
    model: mat4x4<f32>,
    color: vec4<f32>,
    flags: vec4<u32>,
};

@group(0) @binding(0) var<uniform> u: U;
// Same R32Float mirror of the opaque depth attachment that the init
// shader uses. `textureLoad` rather than sampling because Naga's
// WebGL2 backend can't translate `textureLoad` on depth textures.
@group(0) @binding(1) var opaque_depth_color: texture_2d<f32>;
@group(0) @binding(2) var src_dual_depth: texture_2d<f32>;
@group(1) @binding(0) var<uniform> b: B;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) view_pos: vec3<f32>,
    @location(1) v_color: vec4<f32>,
};

@vertex
fn vs(
    @location(0) pos: vec3<f32>,
    @location(1) _normal: vec3<f32>,
    @location(2) v_color: vec4<f32>,
) -> VOut {
    var o: VOut;
    let world_pos4 = b.model * vec4<f32>(pos, 1.0);
    let view_pos4 = u.view * world_pos4;
    o.view_pos = view_pos4.xyz;
    o.clip = u.proj * view_pos4;
    o.v_color = v_color;
    return o;
}

struct PeelOut {
    @location(0) depth_range: vec4<f32>, // (-frontZ, backZ, 0, 0)
    @location(1) front_color: vec4<f32>,
    @location(2) back_color: vec4<f32>,
};

// See the matching constant in `DUAL_DEPTH_INIT_SHADER` for the
// rationale — sized for `Rgba16Float` dual-depth precision.
const PEEL_BIAS: f32 = 1e-3;

fn srgb_to_linear(srgb: vec3<f32>) -> vec3<f32> {
    let low = srgb / 12.92;
    let high = pow((srgb + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return mix(low, high, step(vec3<f32>(0.04045), srgb));
}

fn shade(view_pos: vec3<f32>, base_color: vec4<f32>) -> vec4<f32> {
    let base = srgb_to_linear(base_color.rgb);
    let fdx = dpdx(view_pos);
    let fdy = dpdy(view_pos);
    let n = normalize(cross(fdx, fdy));
    let v = normalize(-view_pos);
    let shininess = max(u.params.x, 1.0);

    let l0 = normalize(u.light_dir0.xyz);
    let ndotl0 = max(dot(n, l0), 0.0);
    let ambient0 = u.light_ambient0.rgb * base;
    let diffuse0 = u.light_diffuse0.rgb * base * ndotl0;
    let h0 = normalize(l0 + v);
    let ndoth0 = max(dot(n, h0), 0.0);
    let spec0_active = step(0.0001, ndotl0);
    let specular0 = u.light_specular0.rgb * u.material_specular.rgb
        * pow(ndoth0, shininess) * spec0_active;

    let l1 = normalize(u.light_dir1.xyz);
    let ndotl1 = max(dot(n, l1), 0.0);
    let diffuse1 = u.light_diffuse1.rgb * base * ndotl1;
    let h1 = normalize(l1 + v);
    let ndoth1 = max(dot(n, h1), 0.0);
    let spec1_active = step(0.0001, ndotl1);
    let specular1 = u.light_specular1.rgb * u.material_specular.rgb
        * pow(ndoth1, shininess) * spec1_active;

    let global_amb = u.global_ambient.rgb * base;
    let lit = global_amb + ambient0 + diffuse0 + specular0 + diffuse1 + specular1;
    return vec4<f32>(lit, base_color.a);
}

@fragment
fn fs(in: VOut) -> PeelOut {
    let pixel = vec2<i32>(clamp(in.clip.xy, vec2<f32>(0.0), u.resolution.xy - vec2<f32>(1.0)));
    let opaque_z = textureLoad(opaque_depth_color, pixel, 0).r;
    if (opaque_z < in.clip.z - PEEL_BIAS) {
        discard;
    }

    let previous = textureLoad(src_dual_depth, pixel, 0).rg;
    let front_z = -previous.x;
    let back_z = previous.y;
    let cur_z = in.clip.z;

    var out: PeelOut;
    out.depth_range = vec4<f32>(-1.0, -1.0, 0.0, 0.0);
    out.front_color = vec4<f32>(0.0);
    out.back_color = vec4<f32>(0.0);

    if (cur_z + PEEL_BIAS < front_z || cur_z - PEEL_BIAS > back_z) {
        discard;
    }

    if (cur_z - PEEL_BIAS > front_z && cur_z + PEEL_BIAS < back_z) {
        // Fragment lies strictly inside the slab — keep it for the
        // next iteration by expanding `dst_dual_depth` to include it.
        out.depth_range = vec4<f32>(-cur_z, cur_z, 0.0, 0.0);
        return out;
    }

    // Per-vertex colour (always populated — see the matching note
    // in the opaque shader's `fs`) drives the surface base colour.
    let shaded = shade(in.view_pos, in.v_color);
    if (abs(cur_z - front_z) <= PEEL_BIAS) {
        // Front-layer hit: premultiply (per MatterCAD's UnderBlend).
        out.front_color = vec4<f32>(shaded.rgb * shaded.a, shaded.a);
    } else {
        // Back-layer hit: standard over blend uses straight alpha.
        out.back_color = shaded;
    }
    return out;
}
"#;

/// Full-screen quad resolve. Verbatim WGSL port of
/// [`DualDepthPeelingMath::ResolveForComposition`](../../../../../../MatterCAD/Submodules/agg-sharp/VorticeD3D/DualDepthPeelingMath.cs)
/// (lines 70-97).
///
/// Inputs:
/// * `scene_color` — single-sample opaque-pass texture (`surface_format`).
/// * `front_accum` — premultiplied front accumulation (`Rgba16Float`).
/// * `back_accum`  — straight-alpha back accumulation (`Rgba16Float`).
///
/// The output is the composited opaque-over-back-over-front colour at
/// each pixel, written straight into the renderer's HDR scene composite.
pub const DUAL_PEEL_RESOLVE_SHADER: &str = r#"
@group(0) @binding(0) var scene_color: texture_2d<f32>;
@group(0) @binding(1) var front_accum: texture_2d<f32>;
@group(0) @binding(2) var back_accum: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// Full-screen triangle covering the [-1,1] NDC range. Using three
// vertices (instead of a quad) saves a vertex and avoids overdraw on
// the diagonal seam.
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

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    let scene = textureSample(scene_color, samp, in.uv);
    let front = textureSample(front_accum, samp, in.uv);
    let back  = textureSample(back_accum,  samp, in.uv);

    // Scene comes from the opaque pass — treat its alpha as 1 if it
    // covered the pixel, 0 otherwise. The opaque pass uses transparent
    // clear, so scene.a is already 0 when nothing was rendered, and 1
    // (or the painted alpha) when something was. Keeping the alpha as-is
    // mirrors MatterCAD's ResolveForComposition which takes
    // sceneColor.W directly.
    let scene_a = clamp(scene.a, 0.0, 1.0);
    let remaining = clamp(front.a * (1.0 - back.a), 0.0, 1.0);
    let transparent_a = 1.0 - remaining;
    let combined_a = scene_a + (1.0 - scene_a) * transparent_a;
    let scene_weight = scene_a * remaining;

    let premul_rgb =
        front.rgb
        + front.a * back.rgb
        + scene_weight * scene.rgb;

    if (combined_a <= 1e-6) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(premul_rgb / combined_a, combined_a);
}
"#;
