//! CPU bake of the bed grid texture.
//!
//! Port of NodeDesigner's
//! [`createGridTexture`](../../../../../FDS/NodeDesigner/static/js/node-editor/rendering/grid-mesh.js):
//! a transparent canvas with opaque grid lines drawn at integer cell
//! boundaries, premultiplied alpha, mipmapped for clean glancing-angle
//! sampling. The texture is uploaded to the GPU once per theme (the
//! line colour is the only theme-driven input); callers retain the
//! returned [`wgpu::Texture`] across frames.
//!
//! The lines themselves are simple axis-aligned rectangles painted into
//! a `Vec<u8>` rather than rasterised through agg-gui. NodeDesigner
//! does exactly this through a 2-D Canvas, which produces the same
//! pixel-snapped strips at the same divisions — copying that behaviour
//! verbatim keeps the bed visually identical between the two
//! applications.

use wgpu::util::DeviceExt;

/// Side length of the baked texture, in pixels. NodeDesigner uses 2048
/// — same here so the mip pyramid covers every realistic on-screen size
/// (200×200 world units at any zoom).
pub const GRID_TEX_SIZE: u32 = 2048;

/// Number of cells per side. NodeDesigner uses 20 (= 10 mm cells over a
/// 200 mm bed). The shader doesn't care about the unit; what matters is
/// matching the bed-quad's world extent so the lines land on integer
/// world coordinates.
pub const GRID_DIVISIONS: u32 = 20;

/// Stroke width in baked-texture pixels. Three texels at 2048 = a
/// ~0.15% line, matching NodeDesigner's `lineWidth = 3` constant.
pub const GRID_LINE_WIDTH: u32 = 3;

/// Build the baked grid texture plus its full mip chain and upload all
/// levels via `queue.write_texture`. The texture format matches
/// `format` so it can be sampled by a pipeline whose target format is
/// the surface format — caller passes the surface format through.
///
/// `line_color` and `fill_color` are desired sRGB-space colours with
/// straight alpha. We premultiply before storing so the texture can be
/// sampled and blended with premultiplied-alpha blending without a
/// per-fragment re-premultiply.
///
/// `fill_color` covers the whole bed (MatterCAD's
/// `BedShadowTextureRenderer.BuildBaseTexture` fills the texture with
/// `theme.BackgroundColor.WithAlpha(80)` before stroking the grid). It
/// is what makes the bed a *translucent surface* rather than a set of
/// floating lines — the alpha varies across the texture (fill alpha
/// between lines, line alpha on them) so the whole bed blends against
/// whatever is behind it.
pub fn bake_grid_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    line_color: [f32; 4],
    fill_color: [f32; 4],
) -> wgpu::Texture {
    let size = GRID_TEX_SIZE;
    let divisions = GRID_DIVISIONS;
    let line_width = GRID_LINE_WIDTH;

    let mip_count = mip_level_count(size, size);
    let base = paint_grid_rgba(size, divisions, line_width, line_color, fill_color, format);

    let tex = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some("atomartist bed grid"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: mip_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::default(),
        &concat_mip_levels(&base, size, size, mip_count),
    );
    tex
}

/// Number of mip levels needed for a `w × h` texture (`floor(log2(max(w,h))) + 1`).
pub fn mip_level_count(w: u32, h: u32) -> u32 {
    let max = w.max(h).max(1);
    32 - max.leading_zeros()
}

fn paint_grid_rgba(
    size: u32,
    divisions: u32,
    line_width: u32,
    line_color_srgb: [f32; 4],
    fill_color_srgb: [f32; 4],
    format: wgpu::TextureFormat,
) -> Vec<u8> {
    let bgra = matches!(
        format,
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
    );
    // Channel order in the destination buffer.
    let (cr, cg, cb) = if bgra { (2, 1, 0) } else { (0, 1, 2) };

    // Translucent bed fill first, grid lines composited over it — same
    // order as MatterCAD's `BuildBaseTexture` (FillRectangle then
    // Line). The fill is what gives the bed its varying alpha.
    let fill = premultiplied_bytes(fill_color_srgb);
    let mut buf = vec![0u8; (size * size * 4) as usize];
    for texel in buf.chunks_exact_mut(4) {
        texel[cr] = fill[0];
        texel[cg] = fill[1];
        texel[cb] = fill[2];
        texel[3] = fill[3];
    }

    // Source-over in premultiplied space: a partially transparent grid
    // line tints the fill instead of punching a hole through it.
    let line = premultiplied_bytes(line_color_srgb);
    let [pr, pg, pb, pa] = over_premultiplied(line, fill);

    let cell = size as f32 / divisions as f32;
    let max_start = size.saturating_sub(line_width);
    for i in 0..=divisions {
        let pos = ((i as f32) * cell).floor() as u32;
        let pos = pos.min(max_start);
        // Vertical line at column `pos` — line_width texels wide, full height.
        for y in 0..size {
            for x in pos..(pos + line_width).min(size) {
                let idx = ((y * size + x) * 4) as usize;
                buf[idx + cr] = pr;
                buf[idx + cg] = pg;
                buf[idx + cb] = pb;
                buf[idx + 3] = pa;
            }
        }
        // Horizontal line at row `pos` — full width, line_width texels tall.
        for y in pos..(pos + line_width).min(size) {
            let row = (y * size * 4) as usize;
            for x in 0..size {
                let idx = row + (x * 4) as usize;
                buf[idx + cr] = pr;
                buf[idx + cg] = pg;
                buf[idx + cb] = pb;
                buf[idx + 3] = pa;
            }
        }
    }
    buf
}

/// Convert a straight-alpha sRGB colour into premultiplied 8-bit bytes.
/// We assume the input is already in display sRGB space (matches the
/// hard-coded constants in [`crate::scene_renderer::WgpuSceneRenderer`])
/// and only need to premultiply by alpha — the colour-space conversion
/// happens at the final framebuffer write, where the wgpu pipeline's
/// sRGB-encoded target takes care of it.
fn premultiplied_bytes(c: [f32; 4]) -> [u8; 4] {
    let a = c[3].clamp(0.0, 1.0);
    let r = (c[0].clamp(0.0, 1.0) * a * 255.0).round() as u8;
    let g = (c[1].clamp(0.0, 1.0) * a * 255.0).round() as u8;
    let b = (c[2].clamp(0.0, 1.0) * a * 255.0).round() as u8;
    let alpha = (a * 255.0).round() as u8;
    [r, g, b, alpha]
}

/// Composite premultiplied `src` over premultiplied `dst`:
/// `out = src + dst * (1 - src.a)`. Both inputs and the result are
/// 8-bit premultiplied RGBA in the same channel order.
fn over_premultiplied(src: [u8; 4], dst: [u8; 4]) -> [u8; 4] {
    let inv = 255u32 - src[3] as u32;
    let mut out = [0u8; 4];
    for c in 0..4 {
        // +127 rounds the fixed-point divide to nearest.
        let v = src[c] as u32 + (dst[c] as u32 * inv + 127) / 255;
        out[c] = v.min(255) as u8;
    }
    out
}

/// Build the full mip chain by repeatedly box-downsampling and
/// concatenate the bytes for `create_texture_with_data`.
fn concat_mip_levels(base: &[u8], w: u32, h: u32, mip_count: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(base.len() * 2);
    out.extend_from_slice(base);
    let mut cur = base.to_vec();
    let mut cur_w = w;
    let mut cur_h = h;
    for _ in 1..mip_count {
        let next_w = (cur_w / 2).max(1);
        let next_h = (cur_h / 2).max(1);
        let next = downsample_rgba_box(&cur, cur_w, cur_h, next_w, next_h);
        out.extend_from_slice(&next);
        cur = next;
        cur_w = next_w;
        cur_h = next_h;
    }
    out
}

/// 2×2 box filter — identical kernel to the helper in
/// `tumble_cube::renderer`. Local copy keeps the bed module
/// self-contained.
fn downsample_rgba_box(src: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    let mut dst = vec![0u8; (dst_w * dst_h * 4) as usize];
    for y in 0..dst_h {
        for x in 0..dst_w {
            let sx0 = (x * 2).min(src_w - 1);
            let sy0 = (y * 2).min(src_h - 1);
            let sx1 = (sx0 + 1).min(src_w - 1);
            let sy1 = (sy0 + 1).min(src_h - 1);
            let mut acc = [0u32; 4];
            for (sx, sy) in [(sx0, sy0), (sx1, sy0), (sx0, sy1), (sx1, sy1)] {
                let i = ((sy * src_w + sx) * 4) as usize;
                for c in 0..4 {
                    acc[c] += src[i + c] as u32;
                }
            }
            let di = ((y * dst_w + x) * 4) as usize;
            for c in 0..4 {
                dst[di + c] = ((acc[c] + 2) / 4) as u8;
            }
        }
    }
    dst
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mip_count_pow2() {
        assert_eq!(mip_level_count(2048, 2048), 12); // 2048 -> 1
        assert_eq!(mip_level_count(1024, 1024), 11);
        assert_eq!(mip_level_count(1, 1), 1);
    }

    #[test]
    fn paint_grid_has_lines_on_cell_boundaries() {
        let size = 64u32;
        let divisions = 8u32;
        let line_width = 2u32;
        let color = [1.0, 1.0, 1.0, 1.0];
        let buf = paint_grid_rgba(
            size,
            divisions,
            line_width,
            color,
            [0.0, 0.0, 0.0, 0.0],
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        // (0,0) is on the first horizontal+vertical line — opaque.
        let i0 = 0usize;
        assert_eq!(buf[i0 + 3], 255);
        // Centre of cell (1,1) at (size/divisions * 1.5) = 12 — should
        // be empty when there is no bed fill.
        let cx = 12u32;
        let cy = 12u32;
        let ic = ((cy * size + cx) * 4) as usize;
        assert_eq!(buf[ic + 3], 0);
        // Final line clamped to size - line_width so it stays in-frame:
        // pixel (size-1, 0) should be opaque (within the last vertical
        // line spanning [size-2, size-1]).
        let last_x = size - 1;
        let il = ((0 * size + last_x) * 4) as usize;
        assert_eq!(buf[il + 3], 255);
    }

    /// MatterCAD's bed is a *translucent surface*, not floating lines:
    /// `BuildBaseTexture` fills the whole texture with the theme
    /// background at alpha 80/255 and strokes the grid over it. The
    /// baked texture must therefore have a non-zero alpha everywhere,
    /// higher on the lines than between them, so the whole bed blends
    /// with what is behind it.
    #[test]
    fn bed_fill_gives_the_whole_texture_varying_alpha() {
        let size = 64u32;
        let fill_alpha = 80.0 / 255.0;
        let buf = paint_grid_rgba(
            size,
            8,
            2,
            [0.2, 0.2, 0.2, 1.0],
            [1.0, 1.0, 1.0, fill_alpha],
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        // Between the lines: the translucent fill, not a hole.
        let between = ((12 * size + 12) * 4) as usize;
        assert_eq!(buf[between + 3], 80);
        // On a line: fully opaque, so lines still read crisply.
        assert_eq!(buf[3], 255);
    }

    /// A grid line with straight alpha < 1 must tint the fill rather
    /// than punch through it — the "lines blend with the bed" half of
    /// MatterCAD's look.
    #[test]
    fn translucent_line_composites_over_the_fill() {
        let size = 64u32;
        let buf = paint_grid_rgba(
            size,
            8,
            2,
            [0.0, 0.0, 0.0, 0.5],
            [1.0, 1.0, 1.0, 80.0 / 255.0],
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        // Line alpha 0.5 over fill alpha 80/255: 128 + 80*(1-0.5) = 168.
        assert_eq!(buf[3], 168);
        // ...and it is more opaque than the bare fill between lines.
        let between = ((12 * size + 12) * 4) as usize;
        assert!(buf[3] > buf[between + 3]);
    }

    #[test]
    fn over_premultiplied_leaves_dst_alone_for_transparent_src() {
        let dst = [10, 20, 30, 40];
        assert_eq!(over_premultiplied([0, 0, 0, 0], dst), dst);
        // Opaque src fully replaces dst.
        let src = [1, 2, 3, 255];
        assert_eq!(over_premultiplied(src, dst), src);
    }

    #[test]
    fn premultiplied_bytes_round_trip_white() {
        assert_eq!(premultiplied_bytes([1.0, 1.0, 1.0, 1.0]), [255, 255, 255, 255]);
        assert_eq!(premultiplied_bytes([1.0, 1.0, 1.0, 0.5]), [128, 128, 128, 128]);
        assert_eq!(premultiplied_bytes([0.5, 0.5, 0.5, 1.0]), [128, 128, 128, 255]);
        assert_eq!(premultiplied_bytes([0.0, 0.0, 0.0, 0.0]), [0, 0, 0, 0]);
    }
}
