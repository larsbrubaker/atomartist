//! WGSL shader source for the opaque scene pass.
//!
//! Kept separate from pipeline setup so [`super::opaque_pass`] stays
//! focused on resource wiring and so the shader text remains readable
//! and uncompressed. Depth-peeling shaders live under
//! `scene_renderer::depth_peel::shaders`; selection-outline shaders
//! live under `scene_renderer::post_outline::shaders`.

// The opaque pass writes a two-attachment FsOut: `@location(0)` is
// the regular shaded colour, `@location(1)` mirrors `in.clip.z` into
// the R32Float "depth-as-colour" attachment that the dual-peel chain
// samples.
//
// Why two attachments instead of sampling the depth attachment:
// Naga's GLSL backend binds `texture_depth_2d` as `sampler2DShadow`,
// and WebGL2 / GLES 3.0 supports neither `texelFetch` nor `textureLod`
// against shadow samplers. Mirroring depth into a regular R32Float
// colour texture sidesteps the limitation on every backend at the
// cost of a single extra fragment-shader output.
//
// Shading matches the dual-peel colour shader and MatterCAD's
// `ApplyLighting`, so an opaque body and a translucent one shade
// identically:
//
// * View-space lighting against the mesh's per-vertex normal
//   (transformed by model→view in the vertex stage). No screen-space
//   derivatives — a derivative-reconstructed normal spikes at triangle
//   edges under perspective and streaks the surface.
// * A global ambient plus two camera-fixed directional lights, each
//   contributing per-light ambient + diffuse. No specular.
// * sRGB-encoded base colour: the shader converts sRGB → linear before
//   lighting and outputs linear (the surface format does the
//   linear → sRGB encode on present).

pub(super) const SCENE_SHADER: &str = r#"
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
    base_color: vec4<f32>,    // fallback when no bodies are bound
    params: vec4<f32>,        // x = shininess
    resolution: vec4<f32>,    // xy = pixel size, zw = pad
};

// Per-body uniform — selected via the dynamic-offset bind group at
// group(1). One slot per renderer body; the caller passes the slot's
// byte offset on every draw. See `body_uniform.rs` for the host-side
// layout and `BodyUniformBuffer` for the storage strategy.
struct B {
    model: mat4x4<f32>,
    color: vec4<f32>,
    flags: vec4<u32>,         // x = use_vertex_colors (0 or 1)
};

@group(0) @binding(0) var<uniform> u: U;
@group(1) @binding(0) var<uniform> b: B;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) view_pos: vec3<f32>,
    @location(1) v_color: vec4<f32>,
    @location(2) view_normal: vec3<f32>,
};

struct FsOut {
    @location(0) color: vec4<f32>,
    @location(1) depth_color: vec4<f32>,
};

@vertex
fn vs(
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) v_color: vec4<f32>,
) -> VOut {
    var o: VOut;
    // Apply the per-body model matrix before the camera view so each
    // body can have its own translation / rotation / scale.
    let world_pos4 = b.model * vec4<f32>(pos, 1.0);
    let view_pos4 = u.view * world_pos4;
    o.view_pos = view_pos4.xyz;
    o.clip = u.proj * view_pos4;
    o.v_color = v_color;
    // Per-vertex normal into view space (model then view), matching the
    // dual-peel colour shader and MatterCAD's `mul(Normal, ModelView)`.
    o.view_normal = (u.view * b.model * vec4<f32>(normal, 0.0)).xyz;
    return o;
}

fn srgb_to_linear(srgb: vec3<f32>) -> vec3<f32> {
    let low = srgb / 12.92;
    let high = pow((srgb + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return mix(low, high, step(vec3<f32>(0.04045), srgb));
}

fn shade(base_color_srgb: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    let base = srgb_to_linear(base_color_srgb);

    let l0 = normalize(u.light_dir0.xyz);
    let ndotl0 = max(dot(n, l0), 0.0);
    let lit0 = u.light_ambient0.rgb * base + u.light_diffuse0.rgb * base * ndotl0;

    let l1 = normalize(u.light_dir1.xyz);
    let ndotl1 = max(dot(n, l1), 0.0);
    let lit1 = u.light_diffuse1.rgb * base * ndotl1;

    let global_amb = u.global_ambient.rgb * base;
    return global_amb + lit0 + lit1;
}

@fragment
fn fs(in: VOut) -> FsOut {
    // The per-vertex colour attribute (slot 1) is always populated:
    // when the source body has a `vertex_colors` overlay the buffer
    // carries it verbatim; otherwise the buffer is filled with the
    // body's uniform tint repeated per vertex. Either way the
    // fragment shader uses `v_color` directly — no branch needed.
    // Two-sided lighting, matching the dual-peel colour shader: flip a
    // back face's normal toward the viewer so it never collapses to the
    // dark ambient floor. (Opaque bodies back-face cull, so this is a
    // no-op here — kept so the two shaders shade identically.)
    var nrm = normalize(in.view_normal);
    let vdir = normalize(-in.view_pos);
    if (dot(nrm, vdir) < 0.0) {
        nrm = -nrm;
    }
    let lit = shade(in.v_color.rgb, nrm);
    var out: FsOut;
    out.color = vec4<f32>(lit, in.v_color.a);
    out.depth_color = vec4<f32>(in.clip.z, 0.0, 0.0, 1.0);
    return out;
}
"#;
