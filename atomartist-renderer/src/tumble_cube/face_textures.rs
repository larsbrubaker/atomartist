//! CPU-rasterized RGBA face textures + per-tile hover overlay paint.
//!
//! Each face owns two `Vec<u8>` buffers:
//!   - `source` — the painted-once label (Top / Left / ...) which never
//!     changes after creation.  Used to repaint the active buffer when
//!     hover state resets.
//!   - `active` — the painted-once label PLUS any hover-tile overlays.
//!     Re-uploaded to the GPU whenever the cursor enters / leaves a
//!     different tile.  Mirrors MatterCAD's `TextureData.active`.
//!
//! Tile indices match `DrawMouseHover` in `TumbleCubeControl.cs`:
//!
//! ```text
//!   6 7 8     top of face (Y near +1)
//!   3 4 5
//!   0 1 2     bottom of face (Y near 0)
//! ```
//!
//! The texture is 256×256 because that's the dimension MatterCAD used —
//! still small enough to upload cheaply and large enough that the text
//! reads crisply at the 100 px widget size.

use std::sync::Arc;

use agg_gui::{
    framebuffer::{unpremultiply_rgba_inplace, Framebuffer},
    text::Font,
    Color, GfxCtx,
};

use super::cube_geometry::Face;

/// Side length of each face texture in pixels.  Matches MatterCAD's
/// `ImageBuffer(256, 256)` choice.
pub const TEX_SIZE: u32 = 256;

/// Default cube-face background colour — a near-white with a faint warm
/// cast, identical in spirit to MatterCAD's `theme.BedColor`.
pub fn default_bg() -> Color {
    Color::rgba(0.95, 0.95, 0.93, 1.0)
}

/// Default text colour for the labels.
pub fn default_text_color() -> Color {
    Color::rgba(0.20, 0.22, 0.26, 1.0)
}

/// Default colour for the hover overlay (the "lit" tile background).
/// Translucent so the underlying label still reads.
pub fn default_hover_overlay() -> Color {
    Color::rgba(0.30, 0.55, 0.95, 0.40)
}

/// One face's CPU pixel state.
pub struct FaceTexture {
    pub face: Face,
    /// Painted-once label (never mutated after creation).
    pub source: Vec<u8>,
    /// `source` + any active hover overlay tiles. Re-uploaded to the
    /// GPU when `dirty == true`.
    pub active: Vec<u8>,
    /// True between the moment the hover state changes and the next GPU
    /// upload; the renderer flips it back to `false` after re-uploading.
    pub dirty: bool,
}

/// Build the six face textures with their labels rasterized once.
///
/// Falls back to plain background fills if `font` is `None` (e.g. in
/// tests that don't install a system font).
pub fn build_face_textures(font: Option<&Arc<Font>>) -> [FaceTexture; 6] {
    let mut out: [Option<FaceTexture>; 6] = Default::default();
    for f in Face::ALL.iter() {
        let label = f.label();
        let painted = rasterize_label(label, font);
        let active = painted.clone();
        out[*f as usize] = Some(FaceTexture {
            face: *f,
            source: painted,
            active,
            dirty: true,
        });
    }
    out.map(|o| o.unwrap())
}

/// Paint a single label centred on a TEX_SIZE × TEX_SIZE RGBA8 buffer.
/// Returns the buffer in **straight-alpha**, top-down row order so it
/// can be uploaded to a wgpu texture directly with
/// `bytes_per_row = TEX_SIZE * 4`.
pub fn rasterize_label(label: &str, font: Option<&Arc<Font>>) -> Vec<u8> {
    let mut fb = Framebuffer::new(TEX_SIZE, TEX_SIZE);
    {
        let mut g = GfxCtx::new(&mut fb);
        // Solid background.
        g.set_fill_color(default_bg());
        g.begin_path();
        g.rect(0.0, 0.0, TEX_SIZE as f64, TEX_SIZE as f64);
        g.fill();

        // Thin border, MatterCAD-style.
        g.set_stroke_color(Color::rgba(0.55, 0.58, 0.66, 0.55));
        g.set_line_width(4.0);
        g.begin_path();
        g.rect(2.0, 2.0, (TEX_SIZE - 4) as f64, (TEX_SIZE - 4) as f64);
        g.stroke();

        // Label text — centred. Uses set_font/fill_text once a font is
        // available; otherwise the label is omitted and the user sees
        // just the background (acceptable degraded state for tests).
        if let Some(f) = font {
            g.set_font(f.clone());
            let size = 60.0;
            g.set_font_size(size);
            g.set_fill_color(default_text_color());
            // measure_text_metrics returns width / ascent in pixels.
            let metrics = agg_gui::text::measure_text_metrics(f, label, size);
            let cx = TEX_SIZE as f64 * 0.5 - metrics.width * 0.5;
            // Baseline at half-height + half-ascent so the glyph reads
            // visually centred.  agg-gui uses bottom-up Y so a higher Y
            // baseline puts the text higher on the texture.
            let cy = TEX_SIZE as f64 * 0.5 - metrics.ascent * 0.35;
            g.fill_text(label, cx, cy);
        }
    }

    // agg-gui rasterizes premultiplied RGBA in bottom-up row order. We
    // want straight-alpha top-down for the wgpu upload — flip rows and
    // un-premultiply.  This mirrors the conversion the `Label` widget
    // does when backing into a cached texture (see `framebuffer.rs`).
    let mut pixels = fb.pixels_flipped();
    unpremultiply_rgba_inplace(&mut pixels);
    pixels
}

/// Repaint a face's `active` buffer to highlight tile `tile` on top of
/// the pristine `source`.  `tile` follows the 3×3 grid layout described
/// in the module-level comment.
///
/// Highlighting strategy: copy `source` → `active`, then blend
/// `overlay_color` into the rectangle occupied by `tile`.  Cheap because
/// we're touching at most 256·256 / 9 ≈ 7300 pixels per face.
pub fn apply_hover_overlay(face_tex: &mut FaceTexture, tile: u32, overlay: Color) {
    face_tex.active.clone_from(&face_tex.source);
    blend_tile(&mut face_tex.active, tile, overlay);
    face_tex.dirty = true;
}

/// Reset the face's `active` buffer to the pristine label.  Called when
/// the cursor leaves the cube or moves to a different face.
pub fn clear_hover_overlay(face_tex: &mut FaceTexture) {
    face_tex.active.clone_from(&face_tex.source);
    face_tex.dirty = true;
}

/// Source-over blend of a coloured rect onto the active pixel buffer.
/// Pixels are straight-alpha RGBA8 top-down, matching the buffer
/// produced by `rasterize_label`.
fn blend_tile(pixels: &mut [u8], tile: u32, color: Color) {
    let (x0, y0, x1, y1) = tile_rect(tile, TEX_SIZE);
    let r = (color.r * 255.0) as u32;
    let g = (color.g * 255.0) as u32;
    let b = (color.b * 255.0) as u32;
    let a = (color.a * 255.0) as u32;
    if a == 0 {
        return;
    }
    let row_stride = (TEX_SIZE * 4) as usize;
    for y in y0..y1 {
        let row = (y as usize) * row_stride;
        for x in x0..x1 {
            let i = row + (x as usize) * 4;
            // Source-over: out = src*α + dst*(1-α). Straight-alpha.
            let inv = 255 - a;
            let dr = pixels[i] as u32;
            let dg = pixels[i + 1] as u32;
            let db = pixels[i + 2] as u32;
            let da = pixels[i + 3] as u32;
            pixels[i] = ((r * a + dr * inv) / 255) as u8;
            pixels[i + 1] = ((g * a + dg * inv) / 255) as u8;
            pixels[i + 2] = ((b * a + db * inv) / 255) as u8;
            // Alpha output: src-over result alpha.
            pixels[i + 3] = (a + da * inv / 255).min(255) as u8;
        }
    }
}

/// Compute `(x0, y0, x1, y1)` (in top-down pixel coords) of the
/// rectangle occupied by `tile` on a `size × size` texture.  Layout:
///
/// ```text
///   6 7 8       y0 row (top of texture, low pixel-Y in top-down)
///   3 4 5
///   0 1 2       bottom of texture (high pixel-Y in top-down)
/// ```
///
/// Corner tiles (0/2/6/8) occupy 1/4 of the face; edge tiles (1/3/5/7)
/// occupy the middle half of one side; tile 4 is the centre 1/2 square.
/// Same proportions MatterCAD's `DrawMouseHover` switch uses (see
/// `TumbleCubeControl.cs` lines 304-374).
pub fn tile_rect(tile: u32, size: u32) -> (u32, u32, u32, u32) {
    let q = size / 4;
    let three_q = q * 3;
    // We render top-down — tile 0/1/2 sit at the *bottom* of the face
    // (high pixel-Y), tiles 6/7/8 at the top (low pixel-Y).
    match tile {
        // bottom row (visual): 0 1 2 → high pixel-Y
        0 => (0, three_q, q, size),
        1 => (q, three_q, three_q, size),
        2 => (three_q, three_q, size, size),
        // middle row: 3 4 5
        3 => (0, q, q, three_q),
        4 => (q, q, three_q, three_q),
        5 => (three_q, q, size, three_q),
        // top row (visual): 6 7 8 → low pixel-Y
        6 => (0, 0, q, q),
        7 => (q, 0, three_q, q),
        8 => (three_q, 0, size, q),
        // Unknown tile — return an empty rect (no-op blend).
        _ => (0, 0, 0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_rects_partition_the_face() {
        // Sum of areas of all nine tiles equals TEX_SIZE^2 with no
        // overlap. We don't need bit-exact partitioning here, just a
        // structural sanity check.
        let mut total: i64 = 0;
        for t in 0..9 {
            let (x0, y0, x1, y1) = tile_rect(t, TEX_SIZE);
            total += ((x1 - x0) * (y1 - y0)) as i64;
        }
        assert_eq!(total, (TEX_SIZE * TEX_SIZE) as i64);
    }

    #[test]
    fn corner_tile_is_smaller_than_center_tile() {
        let (cx0, cy0, cx1, cy1) = tile_rect(4, TEX_SIZE);
        let (kx0, ky0, kx1, ky1) = tile_rect(0, TEX_SIZE);
        let center_area = (cx1 - cx0) * (cy1 - cy0);
        let corner_area = (kx1 - kx0) * (ky1 - ky0);
        assert!(center_area > corner_area);
    }

    #[test]
    fn rasterize_produces_full_size_buffer() {
        // Without a font, the function still produces a valid
        // background-filled buffer.
        let bytes = rasterize_label("Top", None);
        assert_eq!(bytes.len(), (TEX_SIZE * TEX_SIZE * 4) as usize);
    }
}
