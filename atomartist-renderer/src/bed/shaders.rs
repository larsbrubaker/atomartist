//! WGSL shader sources for the bed (grid + contact-shadow) pipeline.
//!
//! Split into a dedicated module so [`super::shadow`] and [`super::mod`]
//! stay focused on pipeline wiring rather than embedded shader text.
//! Mirrors `scene_shaders.rs` for the rest of the scene renderer.
//!
//! Each shader is a port of the matching three.js / GLSL shader in
//! NodeDesigner's `contact-shadows.js` and `grid-mesh.js`, adapted for
//! WGSL and AtomArtist's wgpu pipeline conventions.

/// Shadow caster shader. Outputs flat opaque black for every fragment of
/// the cast mesh; cleared to transparent so the silhouette's alpha
/// channel describes coverage. Vertex layout matches the main scene
/// shader so the same `vbuf` / `ibuf` are reused.
pub(super) const SHADOW_CASTER_SHADER: &str = r#"
struct U {
    mvp: mat4x4<f32>,
};

// Per-body uniform — same layout as the scene shaders' `B` block;
// see `scene_renderer/body_uniform.rs` for the host-side struct and
// the dynamic-offset binding rationale. Selected via group(1)
// binding(0) so each body's shadow uses its own model matrix.
struct B {
    model: mat4x4<f32>,
    color: vec4<f32>,
    flags: vec4<u32>,
};

@group(0) @binding(0) var<uniform> u: U;
@group(1) @binding(0) var<uniform> b: B;

@vertex
fn vs(
    @location(0) pos: vec3<f32>,
    @location(1) _normal: vec3<f32>,
    // Shadow caster doesn't read per-vertex colour but the layout
    // must match the rest of the per-body pipelines — see the
    // init shader's matching `_v_color` comment.
    @location(2) _v_color: vec4<f32>,
) -> @builtin(position) vec4<f32> {
    // Apply the body's model BEFORE the shadow projection so each
    // body casts a shadow at its world-space position.
    return u.mvp * b.model * vec4<f32>(pos, 1.0);
}

@fragment
fn fs() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}
"#;

/// Separable 9-tap Gaussian blur shader. Used twice per frame — once
/// horizontal then once vertical — to smear the silhouette into a soft
/// shadow. Weights and radius are ported verbatim from
/// `contact-shadows.js` so the look matches NodeDesigner exactly.
pub(super) const BLUR_SHADER: &str = r#"
struct U {
    /// Inverse texel size in pixels (1.0 / resolution).
    inv_resolution: vec2<f32>,
    /// (1.0, 0.0) for horizontal, (0.0, 1.0) for vertical.
    direction: vec2<f32>,
    /// Blur radius in pixels — currently 4.0.
    radius: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var src_smp: sampler;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>) -> VOut {
    var o: VOut;
    o.clip = vec4<f32>(pos, 0.0, 1.0);
    o.uv = uv;
    return o;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    let offset: vec2<f32> = u.direction * u.inv_resolution * u.radius;
    // 9-tap Gaussian — five unique weights, mirrored across centre tap.
    var w: array<f32, 5>;
    w[0] = 0.227027;
    w[1] = 0.1945946;
    w[2] = 0.1216216;
    w[3] = 0.054054;
    w[4] = 0.016216;
    var result: vec4<f32> = textureSample(src_tex, src_smp, in.uv) * w[0];
    for (var i: i32 = 1; i < 5; i = i + 1) {
        let off = offset * f32(i) / 4.0;
        result = result + textureSample(src_tex, src_smp, in.uv + off) * w[i];
        result = result + textureSample(src_tex, src_smp, in.uv - off) * w[i];
    }
    return result;
}
"#;

/// Composite shader — combines the baked grid texture with the blurred
/// shadow into a single premultiplied-alpha texture that the bed quad
/// samples. Ports `contact-shadows.js`'s composite material:
///   * `invertShadow = 0.0` → black shadows on a light background
///   * `invertShadow = 1.0` → white shadows on a dark background
pub(super) const COMPOSITE_SHADER: &str = r#"
struct U {
    /// xy: shadow opacity, invert flag (0.0 = light mode, 1.0 = dark)
    /// zw: padding for std140 alignment
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var grid_tex: texture_2d<f32>;
@group(0) @binding(2) var grid_smp: sampler;
@group(0) @binding(3) var shadow_tex: texture_2d<f32>;
@group(0) @binding(4) var shadow_smp: sampler;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>) -> VOut {
    var o: VOut;
    o.clip = vec4<f32>(pos, 0.0, 1.0);
    o.uv = uv;
    return o;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    let grid = textureSample(grid_tex, grid_smp, in.uv);
    let shadow = textureSample(shadow_tex, shadow_smp, in.uv);

    let shadow_opacity = u.params.x;
    let invert = u.params.y;
    let shadow_amount = shadow.a * shadow_opacity;
    // Light mode (invert=0) → black shadows; dark mode (invert=1) → white.
    let shadow_color = mix(vec3<f32>(0.0), vec3<f32>(1.0), invert);

    // "Grid over shadow" source-over in PREMULTIPLIED space. The grid
    // texture is already premultiplied (see
    // `bed::texture::premultiplied_bytes`), so the source term is
    // `grid.rgb`, NOT `grid.rgb * grid.a` — multiplying again darkened
    // and washed out every translucent grid line and the bed fill.
    var result_color: vec3<f32> = shadow_color * shadow_amount;
    var result_alpha: f32 = shadow_amount;
    result_color = grid.rgb + result_color * (1.0 - grid.a);
    result_alpha = grid.a + result_alpha * (1.0 - grid.a);
    return vec4<f32>(result_color, result_alpha);
}
"#;

/// Bed-quad shader — final draw of the floor into the offscreen scene
/// framebuffer. Samples the composite texture and writes premultiplied
/// alpha that blends cleanly with the model and outline passes that
/// follow.
pub(super) const BED_QUAD_SHADER: &str = r#"
struct U {
    mvp: mat4x4<f32>,
    /// xy half-extents of the quad in world units; z = world-Z of the
    /// plane (matches the legacy GridUniforms.cell.z). w = pad.
    plane: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var bed_tex: texture_2d<f32>;
@group(0) @binding(2) var bed_smp: sampler;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>) -> VOut {
    var o: VOut;
    // The vertex buffer is a unit quad on XY in [-1, 1]; splat it out
    // to plane.xy half-extents in world coords. The bed is fixed at
    // the world origin (matching MatterCAD / NodeDesigner) so no pan
    // offset is applied here.
    let world = vec3<f32>(
        pos.x * u.plane.x,
        pos.y * u.plane.y,
        u.plane.z,
    );
    o.clip = u.mvp * vec4<f32>(world, 1.0);
    o.uv = uv;
    return o;
}

// The pass the bed draws into carries a second R32Float attachment
// (the dual-peel chain's opaque-depth reference — see
// `opaque_shaders`). The bed is transparent and deliberately stays
// OUT of that reference, so the slot is written with a dummy value
// and masked off in the pipeline's `write_mask`. The declaration is
// still required because the pass binds two colour attachments.
struct FsOut {
    @location(0) color: vec4<f32>,
    @location(1) depth_color: vec4<f32>,
};

@fragment
fn fs(in: VOut) -> FsOut {
    // Premultiplied sample, blended straight through with
    // premultiplied-alpha blending. There is no alpha test: MatterCAD's
    // bed is a translucent surface whose alpha varies across the
    // texture (fill between lines, opaque on lines, shadow on top), and
    // discarding low-alpha texels would carve the soft edges of the
    // contact shadow out of the bed.
    var out: FsOut;
    out.color = textureSample(bed_tex, bed_smp, in.uv);
    out.depth_color = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    return out;
}
"#;
