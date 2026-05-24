//! Small helpers used by the scene renderer — extracted from
//! `scene_renderer/mod.rs` to keep that file under the 800-line
//! guardrail. Nothing here owns rendering state; these are stateless
//! utilities that the impl blocks call directly.

use super::SAMPLE_COUNT;

/// Format of the auxiliary "depth-as-colour" attachment that mirrors
/// the opaque-pass depth into a sampleable colour texture. R32Float
/// is the smallest single-channel float format every wgpu backend
/// (including WebGL2 / GLES 3.0) supports as both a render attachment
/// AND a sampled `texture_2d<f32>`. We can't just sample the depth
/// attachment directly because Naga's GLSL backend binds depth
/// textures as `sampler2DShadow`, which neither `textureLoad` nor
/// `textureLod` work against in WebGL2.
pub(super) const SCENE_DEPTH_COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;

#[inline]
pub(super) fn ensure_scene_depth(
    device: &wgpu::Device,
    slot: &mut Option<(wgpu::Texture, wgpu::TextureView)>,
    w: u32,
    h: u32,
) {
    if let Some((tex, _)) = slot {
        if tex.width() == w && tex.height() == h {
            return;
        }
    }
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("atomartist scene_depth"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: SAMPLE_COUNT,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        // Used as the depth attachment only — the dual-peel chain
        // samples the mirrored R32Float colour attachment instead
        // (see `ensure_scene_depth_color`).
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    *slot = Some((tex, view));
}

/// Allocate (or reuse) the auxiliary R32Float "depth-as-colour"
/// attachment used by the dual-peel shaders. The opaque-pass
/// pipelines write `clip.z` into this texture's red channel from
/// their fragment shader at `@location(1)`; the peel init / colour
/// shaders then `textureLoad` it as a regular `texture_2d<f32>`,
/// which translates cleanly to `texelFetch(sampler2D, …)` on every
/// backend Naga supports (including WebGL2).
#[inline]
pub(super) fn ensure_scene_depth_color(
    device: &wgpu::Device,
    slot: &mut Option<(wgpu::Texture, wgpu::TextureView)>,
    w: u32,
    h: u32,
) {
    if let Some((tex, _)) = slot {
        if tex.width() == w && tex.height() == h {
            return;
        }
    }
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("atomartist scene_depth_color"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: SAMPLE_COUNT,
        dimension: wgpu::TextureDimension::D2,
        format: SCENE_DEPTH_COLOR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    *slot = Some((tex, view));
}

pub(super) fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-12);
    [v[0] / l, v[1] / l, v[2] / l]
}
