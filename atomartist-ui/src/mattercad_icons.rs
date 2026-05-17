// Bundled MatterCAD icon PNGs for the viewport HUD.
//
// Each constant is the raw 16×16 (or 12×12) RGBA-encoded PNG MatterCAD
// loads via `StaticData.Instance.LoadIcon(…)`. We embed the bytes with
// `include_bytes!`, decode them lazily into straight-alpha RGBA8, and
// recolour them to the theme text colour the same way MatterCAD does
// with `WhiteToAlpha_GreyToColor(theme.TextColor)` — i.e. the original
// luminance modulates alpha and the RGB channels are replaced with the
// theme colour.
//
// The icons are © MatterHackers / John Lewin / Lars Brubaker under the
// MatterCAD FreeBSD (BSD 2-clause) license. They are redistributed in
// AtomArtist (MIT) per that license's terms; see
// `assets/mattercad_icons/LICENSE.txt` for the original notice.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Identity of one bundled icon. The variant selects the PNG byte
/// slice; the rest of the system treats this as `Copy` and uses it
/// for cache keying.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MatterCadIcon {
    Home,
    Cog,
    AngleRight,
    Select,
    Spin,
    Perspective,
    Bed,
    PrintArea,
    PartSelect,
    Rotate,
    Translate,
    Scale,
}

const HOME_PNG: &[u8] = include_bytes!("../assets/mattercad_icons/fa-home_16.png");
const COG_PNG: &[u8] = include_bytes!("../assets/mattercad_icons/fa-cog_16.png");
const ANGLE_RIGHT_PNG: &[u8] = include_bytes!("../assets/mattercad_icons/fa-angle-right_12.png");
const SELECT_PNG: &[u8] = include_bytes!("../assets/mattercad_icons/select.png");
const SPIN_PNG: &[u8] = include_bytes!("../assets/mattercad_icons/spin.png");
const PERSPECTIVE_PNG: &[u8] = include_bytes!("../assets/mattercad_icons/perspective.png");
const BED_PNG: &[u8] = include_bytes!("../assets/mattercad_icons/bed.png");
const PRINT_AREA_PNG: &[u8] = include_bytes!("../assets/mattercad_icons/print_area.png");
const PART_SELECT_PNG: &[u8] =
    include_bytes!("../assets/mattercad_icons/ViewTransformControls/partSelect.png");
const ROTATE_PNG: &[u8] =
    include_bytes!("../assets/mattercad_icons/ViewTransformControls/rotate.png");
const TRANSLATE_PNG: &[u8] =
    include_bytes!("../assets/mattercad_icons/ViewTransformControls/translate.png");
const SCALE_PNG: &[u8] =
    include_bytes!("../assets/mattercad_icons/ViewTransformControls/scale.png");

impl MatterCadIcon {
    fn png_bytes(self) -> &'static [u8] {
        match self {
            MatterCadIcon::Home => HOME_PNG,
            MatterCadIcon::Cog => COG_PNG,
            MatterCadIcon::AngleRight => ANGLE_RIGHT_PNG,
            MatterCadIcon::Select => SELECT_PNG,
            MatterCadIcon::Spin => SPIN_PNG,
            MatterCadIcon::Perspective => PERSPECTIVE_PNG,
            MatterCadIcon::Bed => BED_PNG,
            MatterCadIcon::PrintArea => PRINT_AREA_PNG,
            MatterCadIcon::PartSelect => PART_SELECT_PNG,
            MatterCadIcon::Rotate => ROTATE_PNG,
            MatterCadIcon::Translate => TRANSLATE_PNG,
            MatterCadIcon::Scale => SCALE_PNG,
        }
    }
}

/// Decoded straight-alpha RGBA8 pixels for one icon plus its size.
/// Top-down row order so it can feed `DrawCtx::draw_image_rgba`
/// directly.
#[derive(Clone)]
pub struct DecodedIcon {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Decode the bundled PNG behind `icon`. Cached after the first call
/// per process.
pub fn decoded(icon: MatterCadIcon) -> &'static DecodedIcon {
    static CACHE: OnceLock<Mutex<HashMap<MatterCadIcon, &'static DecodedIcon>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(d) = cache.lock().unwrap().get(&icon).copied() {
        return d;
    }
    let decoded = Box::leak(Box::new(decode_png(icon.png_bytes())));
    cache.lock().unwrap().insert(icon, decoded);
    decoded
}

fn decode_png(bytes: &[u8]) -> DecodedIcon {
    let decoder = png::Decoder::new(bytes);
    let mut reader = decoder.read_info().expect("decode MatterCAD icon PNG header");
    let info = reader.info().clone();
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buf).expect("decode MatterCAD icon PNG body");
    let bytes = &buf[..frame.buffer_size()];
    let width = info.width;
    let height = info.height;
    // Normalise to straight-alpha RGBA8.
    let rgba = match info.color_type {
        png::ColorType::Rgba => bytes.to_vec(),
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(bytes.len() / 3 * 4);
            for chunk in bytes.chunks_exact(3) {
                out.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity(bytes.len() / 2 * 4);
            for chunk in bytes.chunks_exact(2) {
                out.extend_from_slice(&[chunk[0], chunk[0], chunk[0], chunk[1]]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity(bytes.len() * 4);
            for &g in bytes {
                out.extend_from_slice(&[g, g, g, 255]);
            }
            out
        }
        png::ColorType::Indexed => panic!("Indexed-colour PNGs are not used in the bundled set"),
    };
    DecodedIcon { width, height, rgba }
}

/// Apply MatterCAD's `WhiteToAlpha_GreyToColor(target)` recolour to a
/// freshly-decoded icon buffer.  The luminance of the source pixel
/// becomes the inverse of the new alpha (white → transparent, black
/// → opaque) and the RGB is replaced with the target colour.
///
/// Returns a fresh RGBA8 buffer top-down so callers can feed it to
/// `draw_image_rgba` directly.
pub fn tinted_rgba(icon: MatterCadIcon, target_rgb: [u8; 3]) -> Vec<u8> {
    let d = decoded(icon);
    let mut out = Vec::with_capacity(d.rgba.len());
    for px in d.rgba.chunks_exact(4) {
        let r = px[0] as u32;
        let g = px[1] as u32;
        let b = px[2] as u32;
        let a = px[3] as u32;
        // MatterCAD's `WhiteToAlpha_GreyToColor`: luminance → 1 - α,
        // then multiply by the source alpha so transparent input
        // stays transparent.
        let lum = (r * 30 + g * 59 + b * 11) / 100;
        let mask = 255 - lum;
        let new_a = (mask * a) / 255;
        out.extend_from_slice(&[
            target_rgb[0],
            target_rgb[1],
            target_rgb[2],
            new_a as u8,
        ]);
    }
    out
}

/// Cached [`Arc`] variant of [`tinted_rgba`].  Backends are free to
/// defer image draw commands, and the wgpu path benefits from pointer
/// stable image buffers for texture caching, so HUD widgets should use
/// this instead of allocating a fresh `Vec<u8>` during every paint.
pub fn tinted_rgba_arc(icon: MatterCadIcon, target_rgb: [u8; 3]) -> Arc<Vec<u8>> {
    static CACHE: OnceLock<Mutex<HashMap<(MatterCadIcon, [u8; 3]), Arc<Vec<u8>>>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(existing) = cache.lock().unwrap().get(&(icon, target_rgb)).cloned() {
        return existing;
    }
    let rgba = Arc::new(tinted_rgba(icon, target_rgb));
    cache.lock().unwrap().insert((icon, target_rgb), rgba.clone());
    rgba
}

/// Cached scaled+tinted icon buffer. The PNG is first recoloured using
/// [`tinted_rgba`] and then resampled into a fixed-size RGBA8
/// backbuffer. Callers should blit the returned buffer 1:1 to avoid
/// backend-specific runtime scaling differences.
///
/// The resampler uses a cubic spline kernel with support radius 2,
/// matching the spline-family filters AGG uses for high-quality image
/// scaling. This gives stable, smooth icon enlargement for the 1.2x
/// viewport HUD request.
pub fn scaled_tinted_rgba_arc(
    icon: MatterCadIcon,
    target_rgb: [u8; 3],
    scale: f64,
) -> (Arc<Vec<u8>>, u32, u32) {
    scaled_tinted_rgba_arc_to_size(
        icon,
        target_rgb,
        ((decoded(icon).width as f64) * scale).round().max(1.0) as u32,
        ((decoded(icon).height as f64) * scale).round().max(1.0) as u32,
    )
}

/// Cached scaled+tinted icon buffer at an explicit output size.
/// Use this when matching MatterCAD's `LoadIcon(path, 16, 16)`
/// behaviour: some source PNGs are 32×32 or 64×64 but MatterCAD first
/// normalizes them to 16×16 before display.
pub fn scaled_tinted_rgba_arc_to_size(
    icon: MatterCadIcon,
    target_rgb: [u8; 3],
    out_w: u32,
    out_h: u32,
) -> (Arc<Vec<u8>>, u32, u32) {
    static CACHE: OnceLock<Mutex<HashMap<(MatterCadIcon, [u8; 3], u32, u32), Arc<Vec<u8>>>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(existing) = cache
        .lock()
        .unwrap()
        .get(&(icon, target_rgb, out_w, out_h))
        .cloned()
    {
        return (existing, out_w, out_h);
    }

    let tinted = tinted_rgba(icon, target_rgb);
    let d = decoded(icon);
    let scaled = Arc::new(resize_rgba_spline16(
        &tinted,
        d.width,
        d.height,
        out_w,
        out_h,
    ));
    cache
        .lock()
        .unwrap()
        .insert((icon, target_rgb, out_w, out_h), scaled.clone());
    (scaled, out_w, out_h)
}

fn resize_rgba_spline16(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    if sw == dw && sh == dh {
        return src.to_vec();
    }
    let mut dst = vec![0u8; (dw * dh * 4) as usize];
    let scale_x = sw as f64 / dw as f64;
    let scale_y = sh as f64 / dh as f64;
    for y in 0..dh {
        let sy = (y as f64 + 0.5) * scale_y - 0.5;
        let iy = sy.floor() as i32;
        for x in 0..dw {
            let sx = (x as f64 + 0.5) * scale_x - 0.5;
            let ix = sx.floor() as i32;
            let mut acc = [0.0; 4];
            let mut weight_sum = 0.0;
            for yy in (iy - 1)..=(iy + 2) {
                let wy = spline16_weight(sy - yy as f64);
                if wy == 0.0 {
                    continue;
                }
                let cy = yy.clamp(0, sh as i32 - 1) as u32;
                for xx in (ix - 1)..=(ix + 2) {
                    let wx = spline16_weight(sx - xx as f64);
                    if wx == 0.0 {
                        continue;
                    }
                    let w = wx * wy;
                    let cx = xx.clamp(0, sw as i32 - 1) as u32;
                    let si = ((cy * sw + cx) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += src[si + c] as f64 * w;
                    }
                    weight_sum += w;
                }
            }
            let di = ((y * dw + x) * 4) as usize;
            let inv = if weight_sum.abs() < 1e-9 { 1.0 } else { 1.0 / weight_sum };
            for c in 0..4 {
                dst[di + c] = (acc[c] * inv).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    dst
}

/// Cubic spline kernel with support radius 2. The name mirrors AGG's
/// spline-family image filters; this is intentionally separate from
/// `draw_image_rgba` so icons are resampled once into a stable
/// backbuffer and then drawn 1:1.
fn spline16_weight(x: f64) -> f64 {
    let x = x.abs();
    if x < 1.0 {
        (4.0 - 6.0 * x * x + 3.0 * x * x * x) / 6.0
    } else if x < 2.0 {
        let t = 2.0 - x;
        (t * t * t) / 6.0
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_icons_decode_to_nonempty_buffers() {
        for icon in [
            MatterCadIcon::Home,
            MatterCadIcon::Cog,
            MatterCadIcon::AngleRight,
            MatterCadIcon::Select,
            MatterCadIcon::Spin,
            MatterCadIcon::Perspective,
            MatterCadIcon::Bed,
            MatterCadIcon::PrintArea,
            MatterCadIcon::PartSelect,
            MatterCadIcon::Rotate,
            MatterCadIcon::Translate,
            MatterCadIcon::Scale,
        ] {
            let d = decoded(icon);
            assert!(d.width > 0 && d.height > 0, "icon {:?} has 0 dims", icon);
            assert_eq!(
                d.rgba.len() as u32,
                d.width * d.height * 4,
                "icon {:?} byte count wrong",
                icon
            );
        }
    }

    #[test]
    fn tinted_rgba_uses_target_color() {
        let buf = tinted_rgba(MatterCadIcon::Home, [200, 50, 25]);
        assert!(buf.chunks_exact(4).any(|p| p[0] == 200 && p[1] == 50 && p[2] == 25 && p[3] > 0));
    }

    #[test]
    fn tinted_rgba_arc_is_pointer_stable() {
        let a = tinted_rgba_arc(MatterCadIcon::Home, [1, 2, 3]);
        let b = tinted_rgba_arc(MatterCadIcon::Home, [1, 2, 3]);
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn scaled_tinted_rgba_arc_is_scaled_and_pointer_stable() {
        let (a, w, h) = scaled_tinted_rgba_arc(MatterCadIcon::Home, [1, 2, 3], 1.2);
        let (b, w2, h2) = scaled_tinted_rgba_arc(MatterCadIcon::Home, [1, 2, 3], 1.2);
        assert_eq!((w, h), (w2, h2));
        assert_eq!(a.len() as u32, w * h * 4);
        assert!(w > decoded(MatterCadIcon::Home).width);
        assert!(Arc::ptr_eq(&a, &b));
    }
}
