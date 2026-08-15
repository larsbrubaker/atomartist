//! Project preview thumbnails: turning a captured frame into the
//! `Metadata/thumbnail.png` entry an `.atmr` carries.
//!
//! Two halves live in two places. The *format* half — writing and
//! reading the zip entry — is `atomartist_lib::serialization::thumbnail`.
//! This module is the *image* half: pure, GPU-free functions that crop a
//! captured RGBA frame to the preview aspect, box-downscale it, and
//! encode a PNG. Keeping them here (rather than in the native shell)
//! means both shells share one definition of "what a thumbnail looks
//! like", and the math is unit-testable without a window.
//!
//! The capture itself belongs to the shell: `demo-native` reads the
//! frame back off the GPU asynchronously and drops the encoded PNG into
//! [`crate::AppState::set_thumbnail_png`]. Save then embeds whatever is
//! in that slot — a shell that never fills it (tests, WASM for now)
//! simply writes a project without a preview.
//!
//! Row order: captured frames come back Y-down (wgpu surface
//! convention), which is also PNG's row order, so nothing here flips —
//! the bottom-up convention that governs widget coordinates does not
//! apply to raw framebuffer pixels.

use std::io::Cursor;

/// Preview width in pixels. 4:3 with [`THUMBNAIL_HEIGHT`], matching
/// NodeDesigner's `capturePreviewImage` aspect so previews from either
/// app crop the same way.
pub const THUMBNAIL_WIDTH: u32 = 256;
/// Preview height in pixels.
pub const THUMBNAIL_HEIGHT: u32 = 192;

/// A sub-rectangle of a source image, in pixels, origin top-left (the
/// framebuffer's own convention).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CropRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// The largest centered rectangle of `src_w` × `src_h` whose aspect is
/// `aspect_w : aspect_h`.
///
/// A source wider than the target aspect loses columns from both sides;
/// a taller one loses rows from top and bottom. Returns the full source
/// when it already matches (or when either dimension is zero, so callers
/// don't have to special-case an unrealised surface).
pub fn center_crop_rect(src_w: u32, src_h: u32, aspect_w: u32, aspect_h: u32) -> CropRect {
    if src_w == 0 || src_h == 0 || aspect_w == 0 || aspect_h == 0 {
        return CropRect { x: 0, y: 0, w: src_w, h: src_h };
    }
    // Compare src_w/src_h against aspect_w/aspect_h without floats.
    let src_ratio = src_w as u64 * aspect_h as u64;
    let target_ratio = src_h as u64 * aspect_w as u64;
    if src_ratio > target_ratio {
        // Wider than the target: keep full height, trim width.
        let w = ((src_h as u64 * aspect_w as u64) / aspect_h as u64).max(1) as u32;
        CropRect { x: (src_w - w) / 2, y: 0, w, h: src_h }
    } else if src_ratio < target_ratio {
        // Taller than the target: keep full width, trim height.
        let h = ((src_w as u64 * aspect_h as u64) / aspect_w as u64).max(1) as u32;
        CropRect { x: 0, y: (src_h - h) / 2, w: src_w, h }
    } else {
        CropRect { x: 0, y: 0, w: src_w, h: src_h }
    }
}

/// Convert a widget rectangle into the framebuffer rectangle covering
/// the same pixels, clipped to the surface.
///
/// Two coordinate systems meet here. Widget bounds are agg-gui's
/// **bottom-up** first-quadrant pixels — `y` is the *bottom* edge,
/// measured up from the bottom of the window. Captured frames are
/// **top-down** rows, so the widget's top edge (`y + h`) becomes the
/// first row: `y_fb = surface_h - (y + h)`.
///
/// Both spaces are *physical* pixels: the native shell lays the widget
/// tree out at the surface's own size (`gpu.config.width/height`), so no
/// DPI factor enters here — `agg_gui::set_device_scale` only affects how
/// text is rasterised, not the coordinate space.
///
/// Returns `None` when the result would be empty: a zero-sized or
/// hidden widget, a rect entirely off the surface, an unrealised
/// surface, or non-finite coordinates. Callers treat that as "skip the
/// capture this time".
pub fn framebuffer_crop_from_widget_rect(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    surface_w: u32,
    surface_h: u32,
) -> Option<CropRect> {
    if !(x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite()) {
        return None;
    }
    if w <= 0.0 || h <= 0.0 || surface_w == 0 || surface_h == 0 {
        return None;
    }
    let (sw, sh) = (surface_w as f64, surface_h as f64);
    let x0 = x.floor().max(0.0).min(sw);
    let x1 = (x + w).ceil().max(0.0).min(sw);
    // Flip: the widget's top edge is the framebuffer's first row.
    let y0 = (sh - (y + h)).floor().max(0.0).min(sh);
    let y1 = (sh - y).ceil().max(0.0).min(sh);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(CropRect {
        x: x0 as u32,
        y: y0 as u32,
        w: (x1 - x0) as u32,
        h: (y1 - y0) as u32,
    })
}

/// The largest centered 4:3 rectangle *inside* `region` — the preview
/// crop applied to a sub-rectangle of the frame rather than the whole
/// frame.
fn center_crop_within(region: CropRect, aspect_w: u32, aspect_h: u32) -> CropRect {
    let inner = center_crop_rect(region.w, region.h, aspect_w, aspect_h);
    CropRect {
        x: region.x.saturating_add(inner.x),
        y: region.y.saturating_add(inner.y),
        w: inner.w,
        h: inner.h,
    }
}

/// Box-average `src` (RGBA8, `src_w` × `src_h`) inside `crop` down to
/// `dst_w` × `dst_h`, returning tightly packed RGB8.
///
/// A box filter — not point sampling — because the source is typically
/// 5-8× larger than the preview and nearest-neighbour on a shaded mesh
/// produces obvious stair-stepping. Alpha is dropped: the captured
/// surface is opaque and RGB keeps the embedded PNG smaller.
///
/// Returns `None` when the buffer is too small for the stated size or
/// any dimension is zero.
pub fn downscale_rgba_to_rgb(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    crop: CropRect,
    dst_w: u32,
    dst_h: u32,
) -> Option<Vec<u8>> {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 || crop.w == 0 || crop.h == 0 {
        return None;
    }
    // u64 throughout: `crop` comes from a public API and `usize` is 32
    // bits on wasm32, where a large frame would otherwise overflow the
    // bounds check it is supposed to perform.
    if crop.x as u64 + crop.w as u64 > src_w as u64
        || crop.y as u64 + crop.h as u64 > src_h as u64
    {
        return None;
    }
    let needed = src_w as u64 * src_h as u64 * 4;
    if (src.len() as u64) < needed {
        return None;
    }

    let mut out = Vec::with_capacity((dst_w as usize) * (dst_h as usize) * 3);
    for dy in 0..dst_h {
        // Source row span covered by this destination row.
        let y0 = crop.y + (dy as u64 * crop.h as u64 / dst_h as u64) as u32;
        let y1 = (crop.y + ((dy as u64 + 1) * crop.h as u64 / dst_h as u64) as u32).max(y0 + 1);
        for dx in 0..dst_w {
            let x0 = crop.x + (dx as u64 * crop.w as u64 / dst_w as u64) as u32;
            let x1 = (crop.x + ((dx as u64 + 1) * crop.w as u64 / dst_w as u64) as u32).max(x0 + 1);
            let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
            for y in y0..y1.min(crop.y + crop.h) {
                let row = (y as usize) * (src_w as usize) * 4;
                for x in x0..x1.min(crop.x + crop.w) {
                    let i = row + (x as usize) * 4;
                    r += src[i] as u64;
                    g += src[i + 1] as u64;
                    b += src[i + 2] as u64;
                    n += 1;
                }
            }
            if n == 0 {
                n = 1;
            }
            out.push((r / n) as u8);
            out.push((g / n) as u8);
            out.push((b / n) as u8);
        }
    }
    Some(out)
}

/// Full capture → preview pipeline: center-crop the RGBA frame to 4:3,
/// downscale to [`THUMBNAIL_WIDTH`] × [`THUMBNAIL_HEIGHT`], encode PNG.
///
/// `None` when the frame is unusable (empty, mis-sized) or the encoder
/// fails; callers treat that as "no thumbnail this time", never as an
/// error worth telling the user about.
pub fn thumbnail_png_from_rgba(src: &[u8], src_w: u32, src_h: u32) -> Option<Vec<u8>> {
    let whole = CropRect { x: 0, y: 0, w: src_w, h: src_h };
    thumbnail_png_from_rgba_region(src, src_w, src_h, whole)
}

/// [`thumbnail_png_from_rgba`] restricted to `region` — the shell's
/// 3-D viewport rectangle, so the preview shows the *model* rather than
/// the node canvas and side panels that share the window.
///
/// The 4:3 crop is applied *within* `region`; a region that doesn't fit
/// the frame, or a degenerate one, yields `None`.
pub fn thumbnail_png_from_rgba_region(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    region: CropRect,
) -> Option<Vec<u8>> {
    let crop = center_crop_within(region, THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT);
    let rgb = downscale_rgba_to_rgb(
        src,
        src_w,
        src_h,
        crop,
        THUMBNAIL_WIDTH,
        THUMBNAIL_HEIGHT,
    )?;
    encode_rgb_png(&rgb, THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT)
}

/// Encode tightly packed RGB8 as a PNG. Separate from the resampler so
/// tests can exercise the geometry without going through the codec.
pub fn encode_rgb_png(rgb: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    if rgb.len() != (w as usize) * (h as usize) * 3 {
        return None;
    }
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut encoder = png::Encoder::new(Cursor::new(&mut buf), w, h);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(rgb).ok()?;
    }
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wider_than_target_crops_columns_from_both_sides() {
        // 1920x1080 (16:9) into 4:3 keeps full height, width 1440.
        let c = center_crop_rect(1920, 1080, 4, 3);
        assert_eq!(c, CropRect { x: 240, y: 0, w: 1440, h: 1080 });
    }

    #[test]
    fn taller_than_target_crops_rows_from_both_sides() {
        // 600x1000 into 4:3 keeps full width, height 450.
        let c = center_crop_rect(600, 1000, 4, 3);
        assert_eq!(c, CropRect { x: 0, y: 275, w: 600, h: 450 });
    }

    #[test]
    fn exact_aspect_is_left_alone() {
        assert_eq!(
            center_crop_rect(800, 600, 4, 3),
            CropRect { x: 0, y: 0, w: 800, h: 600 }
        );
    }

    #[test]
    fn degenerate_sizes_do_not_panic() {
        assert_eq!(
            center_crop_rect(0, 0, 4, 3),
            CropRect { x: 0, y: 0, w: 0, h: 0 }
        );
        assert!(thumbnail_png_from_rgba(&[], 0, 0).is_none());
        assert!(thumbnail_png_from_rgba(&[0u8; 16], 100, 100).is_none());
    }

    #[test]
    fn widget_rect_flips_bottom_up_y_into_framebuffer_rows() {
        // A 400x300 widget sitting on the bottom-left of an 800x600
        // surface occupies framebuffer rows 300..600.
        assert_eq!(
            framebuffer_crop_from_widget_rect(0.0, 0.0, 400.0, 300.0, 800, 600),
            Some(CropRect { x: 0, y: 300, w: 400, h: 300 })
        );
        // The same widget at the top-left occupies rows 0..300.
        assert_eq!(
            framebuffer_crop_from_widget_rect(0.0, 300.0, 400.0, 300.0, 800, 600),
            Some(CropRect { x: 0, y: 0, w: 400, h: 300 })
        );
    }

    #[test]
    fn widget_rect_covering_the_surface_is_the_whole_frame() {
        assert_eq!(
            framebuffer_crop_from_widget_rect(0.0, 0.0, 800.0, 600.0, 800, 600),
            Some(CropRect { x: 0, y: 0, w: 800, h: 600 })
        );
    }

    #[test]
    fn widget_rect_is_clipped_to_the_surface() {
        // Hangs off the right edge and below the bottom: the overlap is
        // what gets captured.
        let c = framebuffer_crop_from_widget_rect(700.0, -100.0, 400.0, 300.0, 800, 600)
            .expect("partial overlap is usable");
        assert_eq!(c, CropRect { x: 700, y: 400, w: 100, h: 200 });
    }

    #[test]
    fn degenerate_or_offscreen_widget_rects_are_rejected() {
        // Hidden viewport (zero size).
        assert!(framebuffer_crop_from_widget_rect(10.0, 10.0, 0.0, 0.0, 800, 600).is_none());
        // Entirely above the surface.
        assert!(framebuffer_crop_from_widget_rect(0.0, 700.0, 100.0, 100.0, 800, 600).is_none());
        // Entirely to the right.
        assert!(framebuffer_crop_from_widget_rect(900.0, 0.0, 100.0, 100.0, 800, 600).is_none());
        // Unrealised surface.
        assert!(framebuffer_crop_from_widget_rect(0.0, 0.0, 100.0, 100.0, 0, 0).is_none());
        // Nonsense coordinates.
        assert!(framebuffer_crop_from_widget_rect(f64::NAN, 0.0, 100.0, 100.0, 800, 600).is_none());
    }

    #[test]
    fn region_pipeline_samples_only_the_requested_rectangle() {
        // 400x400 frame: blue everywhere except a 200x150 green patch
        // (already 4:3, so the center-crop keeps all of it). Capturing
        // that region must produce a purely green preview — proof the
        // panels around the viewport never reach the PNG.
        let (w, h) = (400u32, 400u32);
        let mut src = vec![0u8, 0, 255, 255].repeat((w * h) as usize);
        let region = CropRect { x: 100, y: 40, w: 200, h: 150 };
        for y in region.y..region.y + region.h {
            for x in region.x..region.x + region.w {
                let i = ((y * w + x) * 4) as usize;
                src[i..i + 4].copy_from_slice(&[0, 255, 0, 255]);
            }
        }

        let png_bytes =
            thumbnail_png_from_rgba_region(&src, w, h, region).expect("region thumbnail");
        let decoder = png::Decoder::new(Cursor::new(&png_bytes));
        let mut reader = decoder.read_info().expect("png header");
        assert_eq!(reader.info().width, THUMBNAIL_WIDTH);
        assert_eq!(reader.info().height, THUMBNAIL_HEIGHT);
        let mut out = vec![0u8; reader.output_buffer_size()];
        reader.next_frame(&mut out).expect("png pixels");
        assert!(
            out.chunks_exact(3).all(|p| p == [0, 255, 0]),
            "every preview pixel must come from the viewport region"
        );
    }

    #[test]
    fn region_pipeline_rejects_a_region_outside_the_frame() {
        let src = vec![0u8; 100 * 100 * 4];
        let outside = CropRect { x: 90, y: 90, w: 50, h: 50 };
        assert!(thumbnail_png_from_rgba_region(&src, 100, 100, outside).is_none());
    }

    #[test]
    fn downscale_averages_the_source_block() {
        // 2x2 source, one pixel per quadrant, downscaled to 1x1: the
        // result is the mean of all four.
        let src: Vec<u8> = vec![
            0, 0, 0, 255, 100, 100, 100, 255, // row 0
            200, 200, 200, 255, 255, 255, 255, 255, // row 1
        ];
        let crop = CropRect { x: 0, y: 0, w: 2, h: 2 };
        let out = downscale_rgba_to_rgb(&src, 2, 2, crop, 1, 1).expect("downscale");
        let expected = ((0 + 100 + 200 + 255) / 4) as u8;
        assert_eq!(out, vec![expected, expected, expected]);
    }

    #[test]
    fn thumbnail_pipeline_produces_a_256x192_png() {
        // Solid-red 640x480 frame → decodable 256x192 PNG.
        let src = vec![255u8, 0, 0, 255].repeat(640 * 480);
        let png_bytes = thumbnail_png_from_rgba(&src, 640, 480).expect("encode");
        assert_eq!(&png_bytes[1..4], b"PNG");

        let decoder = png::Decoder::new(Cursor::new(&png_bytes));
        let mut reader = decoder.read_info().expect("png header");
        assert_eq!(reader.info().width, THUMBNAIL_WIDTH);
        assert_eq!(reader.info().height, THUMBNAIL_HEIGHT);
        let mut out = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut out).expect("png pixels");
        assert_eq!(&out[..3], &[255, 0, 0]);
        assert_eq!(info.color_type, png::ColorType::Rgb);
    }
}
