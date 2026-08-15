//! PNG preview decoding for [`super::thumbs`]: embedded image bytes in,
//! a size-fitted [`ThumbnailImage`] out.
//!
//! Split from `thumbs.rs` so both files stay well under the 800-line cap
//! (CLAUDE.md); attached with `#[path]` as a private child of the cache,
//! because nothing else should be decoding preview bytes.
//!
//! Everything here is pure and GPU-free — bytes to pixels, no storage, no
//! state — and defensive by default: the input arrives from whatever
//! package the browser happened to list, so a hostile or simply broken
//! image must produce an answer, never an allocation the machine cannot
//! afford. Row order is top-down throughout (see [`ThumbnailImage`]).

use super::ThumbnailImage;

/// Refuse to decode a preview larger than this many pixels. The zip
/// extractor already caps the *compressed* size, but a few hundred KB of
/// PNG can declare an enormous canvas, and the decoded buffer is what
/// costs memory.
const MAX_DECODE_PIXELS: u64 = 4 * 1024 * 1024;

/// The eight-byte PNG signature. A package may store its preview as JPEG
/// (`Metadata/thumbnail.jpg` is one of the conventional paths); we have no
/// JPEG decoder, and that is an absence, not a failure.
pub(super) const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// Decode an embedded preview to RGBA8 and fit it into a `size`-pixel box.
///
/// `Ok(None)` means "not an image we decode" — a foreign package's JPEG,
/// which is an absence rather than a failure. `Err` is a PNG we should have
/// been able to read and could not, or one whose declared canvas is beyond
/// [`MAX_DECODE_PIXELS`].
pub(super) fn decode_preview(bytes: &[u8], size: u32) -> Result<Option<ThumbnailImage>, String> {
    if !bytes.starts_with(PNG_MAGIC) {
        return Ok(None);
    }
    // The canvas size is checked straight off IHDR, *before* the decoder
    // touches the stream: `read_info` reads on to the first data chunk, so
    // a header that declares a 900-megapixel image would otherwise be
    // reported as some other parse error (or, for a complete file, get as
    // far as sizing a buffer for it).
    if let Some((w, h)) = png_ihdr_dimensions(bytes) {
        if w as u64 * h as u64 > MAX_DECODE_PIXELS {
            return Err(format!("preview is implausibly large ({w}×{h})"));
        }
    }
    let mut decoder = png::Decoder::new(bytes);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    // Second line of defence for anything the header did not reveal
    // (interlacing, a lying IHDR): cap what the decoder may allocate.
    decoder.set_limits(png::Limits {
        bytes: (MAX_DECODE_PIXELS * 4) as usize,
    });
    let mut reader = decoder
        .read_info()
        .map_err(|err| format!("preview header: {err}"))?;
    let (width, height) = {
        let info = reader.info();
        (info.width, info.height)
    };
    if width == 0 || height == 0 {
        return Err("preview has a zero dimension".to_string());
    }
    if width as u64 * height as u64 > MAX_DECODE_PIXELS {
        return Err(format!("preview is implausibly large ({width}×{height})"));
    }

    let mut buf = vec![0u8; reader.output_buffer_size()];
    let frame = reader
        .next_frame(&mut buf)
        .map_err(|err| format!("preview body: {err}"))?;
    let (color, _depth) = reader.output_color_type();
    let pixels = &buf[..frame.buffer_size()];
    let rgba = to_rgba8(pixels, color).ok_or_else(|| format!("unsupported PNG format {color:?}"))?;
    if rgba.len() < (width as usize) * (height as usize) * 4 {
        return Err("preview is shorter than its declared size".to_string());
    }

    let (dst_w, dst_h) = fit_within(width, height, size.max(1));
    let rgba = if dst_w == width && dst_h == height {
        rgba
    } else {
        downscale_rgba(&rgba, width, height, dst_w, dst_h)
    };
    Ok(Some(ThumbnailImage {
        width: dst_w,
        height: dst_h,
        rgba,
    }))
}

/// Width and height straight out of a PNG's IHDR chunk, which a valid file
/// always places immediately after the signature. `None` when the bytes do
/// not have that shape — the decoder then reports the real problem.
fn png_ihdr_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    // 8 signature + 4 length + 4 type + 4 width + 4 height.
    if bytes.len() < 24 || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((w, h))
}

/// Normalise an 8-bit PNG frame to straight-alpha RGBA8. `None` for a
/// colour type the transformations should have removed (indexed, 16-bit),
/// which the caller reports as unsupported rather than guessing.
fn to_rgba8(pixels: &[u8], color: png::ColorType) -> Option<Vec<u8>> {
    let out = match color {
        png::ColorType::Rgba => pixels.to_vec(),
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(pixels.len() / 3 * 4);
            for px in pixels.chunks_exact(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity(pixels.len() / 2 * 4);
            for px in pixels.chunks_exact(2) {
                out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity(pixels.len() * 4);
            for &g in pixels {
                out.extend_from_slice(&[g, g, g, 255]);
            }
            out
        }
        _ => return None,
    };
    Some(out)
}

/// Largest size with the source's aspect ratio whose longest edge is at
/// most `box_px`. An image already inside the box is left alone — previews
/// are small, and upscaling one only costs memory.
pub(super) fn fit_within(w: u32, h: u32, box_px: u32) -> (u32, u32) {
    let longest = w.max(h);
    if longest <= box_px {
        return (w, h);
    }
    let scaled_w = ((w as u64 * box_px as u64) / longest as u64).max(1) as u32;
    let scaled_h = ((h as u64 * box_px as u64) / longest as u64).max(1) as u32;
    (scaled_w, scaled_h)
}

/// Box-filter downscale of an RGBA8 buffer, alpha included.
///
/// A box filter rather than point sampling for the same reason
/// [`crate::thumbnail::downscale_rgba_to_rgb`] uses one — nearest-neighbour
/// on a shaded render stair-steps visibly — but this one keeps alpha, since
/// a browser preview is composited over the row background.
fn downscale_rgba(src: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((dst_w as usize) * (dst_h as usize) * 4);
    for dy in 0..dst_h {
        let y0 = (dy as u64 * src_h as u64 / dst_h as u64) as u32;
        let y1 = (((dy as u64 + 1) * src_h as u64 / dst_h as u64) as u32).max(y0 + 1);
        for dx in 0..dst_w {
            let x0 = (dx as u64 * src_w as u64 / dst_w as u64) as u32;
            let x1 = (((dx as u64 + 1) * src_w as u64 / dst_w as u64) as u32).max(x0 + 1);
            let (mut r, mut g, mut b, mut a, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
            for y in y0..y1.min(src_h) {
                let row = (y as usize) * (src_w as usize) * 4;
                for x in x0..x1.min(src_w) {
                    let i = row + (x as usize) * 4;
                    r += src[i] as u64;
                    g += src[i + 1] as u64;
                    b += src[i + 2] as u64;
                    a += src[i + 3] as u64;
                    n += 1;
                }
            }
            if n == 0 {
                n = 1;
            }
            out.push((r / n) as u8);
            out.push((g / n) as u8);
            out.push((b / n) as u8);
            out.push((a / n) as u8);
        }
    }
    out
}

