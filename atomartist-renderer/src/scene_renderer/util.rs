//! Small helpers used by the scene renderer — extracted from
//! `scene_renderer/mod.rs` to keep that file under the 800-line
//! guardrail. Nothing here owns rendering state; these are stateless
//! utilities that the impl blocks call directly.

use super::SAMPLE_COUNT;

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
        // `TEXTURE_BINDING` so the dual-peel init / colour shaders can
        // sample the opaque-pass depth via `texture_depth_2d`.
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
