//! WGSL shader sources for the opaque scene pass and the inverted-hull
//! outline pass.
//!
//! Kept separate from pipeline setup so [`super::opaque_pass`] stays
//! focused on resource wiring and so the shader text remains readable
//! and uncompressed. Depth-peeling shaders live under
//! `scene_renderer::depth_peel::shaders`; accumulation-blend shaders
//! live under `scene_renderer::accumulation`.

// Every opaque-pass fragment shader writes a two-attachment FsOut:
// `@location(0)` is the regular shaded colour, `@location(1)` mirrors
// `in.clip.z` into the R32Float "depth-as-colour" attachment that
// the dual-peel chain samples. Pipelines that should NOT contribute
// to the peel-reference depth (currently just the inverted-hull
// outline) zero out `@location(1)`'s write mask on their colour
// target — the value the shader returns is discarded.
//
// Why two attachments instead of sampling the depth attachment:
// Naga's GLSL backend binds `texture_depth_2d` as `sampler2DShadow`,
// and WebGL2 / GLES 3.0 supports neither `texelFetch` nor `textureLod`
// against shadow samplers. Mirroring depth into a regular R32Float
// colour texture sidesteps the limitation on every backend at the
// cost of a single extra fragment-shader output.

pub(super) const SCENE_SHADER: &str = r#"
struct Uniforms {
    mvp: mat4x4<f32>,
    normal_mat: mat4x4<f32>,
    light_dir: vec4<f32>,
    base_color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
};

struct FsOut {
    @location(0) color: vec4<f32>,
    @location(1) depth_color: vec4<f32>,
};

@vertex
fn vs(@location(0) pos: vec3<f32>, @location(1) normal: vec3<f32>) -> VOut {
    var o: VOut;
    o.clip = u.mvp * vec4<f32>(pos, 1.0);
    o.world_normal = (u.normal_mat * vec4<f32>(normal, 0.0)).xyz;
    return o;
}

@fragment
fn fs(in: VOut) -> FsOut {
    let n = normalize(in.world_normal);
    let l = normalize(u.light_dir.xyz);
    let diff = max(dot(n, l), 0.0);
    let fill = max(dot(n, vec3<f32>(-l.x, l.y * 0.4, -l.z)), 0.0) * 0.35;
    let ambient = 0.18;
    let lit = ambient + diff * 0.85 + fill;
    let rgb = u.base_color.rgb * lit;
    var out: FsOut;
    out.color = vec4<f32>(rgb, u.base_color.a);
    out.depth_color = vec4<f32>(in.clip.z, 0.0, 0.0, 1.0);
    return out;
}
"#;

pub(super) const OUTLINE_SHADER: &str = r#"
struct U {
    mvp: mat4x4<f32>,
    color: vec4<f32>,
    width: vec4<f32>, // x = world-space inflation distance
};

@group(0) @binding(0) var<uniform> u: U;

struct VOut {
    @builtin(position) clip: vec4<f32>,
};

struct FsOut {
    @location(0) color: vec4<f32>,
    @location(1) depth_color: vec4<f32>,
};

@vertex
fn vs(@location(0) pos: vec3<f32>, @location(1) normal: vec3<f32>) -> VOut {
    var o: VOut;
    let inflated = pos + normalize(normal) * u.width.x;
    o.clip = u.mvp * vec4<f32>(inflated, 1.0);
    return o;
}

@fragment
fn fs(in: VOut) -> FsOut {
    var out: FsOut;
    out.color = u.color;
    // Outline's inflated z must NOT pollute the peel-reference depth
    // — the pipeline zeroes the write mask on `depth_color`, so this
    // value is discarded. We still have to return something for the
    // shader's declared output struct to be valid.
    out.depth_color = vec4<f32>(in.clip.z, 0.0, 0.0, 1.0);
    return out;
}
"#;
