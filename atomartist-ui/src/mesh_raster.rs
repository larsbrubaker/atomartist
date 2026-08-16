//! A tiny software rasterizer that turns a triangle soup into a small
//! RGBA icon (`docs/file-browser-design.md` §5b, step 6f-2).
//!
//! # Why software, and not the wgpu renderer
//!
//! NodeDesigner renders its parts-bar icons through a *second*, offscreen
//! `THREE.WebGLRenderer` (`parts-bar-icons.js`). We deliberately do not
//! copy that: [`atomartist_renderer`] draws into the shell's swapchain and
//! has no headless/offscreen entry point, so reusing it would mean
//! creating a `wgpu::Device` — impossible in the UI-test harness (which is
//! the whole test suite) and asynchronous on wasm, for a job whose entire
//! workload is seven 96 px images of a few hundred triangles each. A
//! perspective projection with a z-buffer is ~200 lines, runs identically
//! on native, wasm and headless, is deterministic (so tests can assert on
//! pixels), and gives per-face normals for free.
//!
//! # Camera and lighting: NodeDesigner's numbers
//!
//! Reproduced from `parts-bar-icons.js` so an AtomArtist icon reads like
//! its ancestor: perspective camera, fov 30°, **Z-up** (matching the 3-D
//! viewport's world), positioned at `center + dir · distance` with
//! `dir = normalize(-0.35, -1, 0.55)` (front-left-above) and
//! `distance = radius / tan(fov·π/360) · 1.15`, looking at the mesh's
//! bounding-sphere centre. Shading is Lambert with ambient `0.55`, a key
//! directional light of intensity `1.6` from `(-40, -60, 80)`, and a fill
//! of `0.5` from `(60, -20, 20)`. Normals are **per face** — the ancestor
//! explicitly de-indexes its geometry to get the faceted look the viewer
//! has, which for us just means "compute the normal from the triangle and
//! never interpolate it".
//!
//! # Colour space: light linearly, write sRGB
//!
//! NodeDesigner's icon renderer is a plain `THREE.WebGLRenderer`, and the
//! app sets `THREE.ColorManagement.enabled = false`
//! (`rendering/three-viewer.js`) — so a `THREE.Color("#fe8d86")` keeps its
//! raw sRGB components, the Lambert term multiplies *those*, and the
//! renderer's default `outputColorSpace = SRGBColorSpace` then encodes the
//! result linear→sRGB on the way to the framebuffer (`getTransfer` ignores
//! the `enabled` flag, so the output encode still runs). The net pipeline
//! is therefore `srgb_encode(tint · lambert)`, not `tint · lambert`.
//!
//! We used to write `tint · lambert` straight out, which is the same
//! picture only where `lambert ≈ 1`; everywhere else it read darker and
//! far more saturated. Measured on a Box (MatterCAD's `Cube` tint,
//! `#FE8D86`), old → new:
//!
//! ```text
//! face            lambert   tint·lambert    srgb_encode(tint·lambert)
//! ambient only      0.55    (140,  78,  74)   (195, 150, 146)
//! -X (key light)    1.14    (255, 161, 153)   (255, 208, 203)
//! -Y (front)        1.59    (255, 225, 213)   (255, 241, 236)
//! ```
//!
//! Average saturation over the covered pixels of the 96 px Box icon
//! halves, 0.15 → 0.07. The encode happens where the supersampled
//! buffer resolves into bytes (`SampleBuffer::resolve`), and it is what
//! makes an AtomArtist icon read as softly as its ancestor's.
//!
//! # Two-sided shading instead of back-face culling
//!
//! The ancestor renders `FrontSide` and relies on every generator winding
//! consistently. We instead flip the face normal toward the eye and let
//! the z-buffer decide: for a closed mesh the result is identical, and a
//! primitive whose winding is reversed (or an imported mesh, if this ever
//! serves the browser's mesh thumbnails) renders as a solid object rather
//! than an inside-out shell.
//!
//! # Coordinates
//!
//! Input triangles are **world space** (the 3-D viewport's right-handed
//! Z-up). The output image is in *PNG row order* — row 0 is the top —
//! which is what `DrawCtx::draw_image_rgba` and the file browser's
//! thumbnails already use. Note this is the one place in the UI crate
//! that is *not* agg-gui's Y-up: it is pixel data, not a widget rectangle.

use std::sync::Arc;

use glam::{Mat4, Vec3, Vec4Swizzles};

/// The ancestor's icon edge length in pixels (`parts-bar-icons.js`'s
/// `ICON_SIZE`). Kept as the reference size the tests render at;
/// on-screen icons are rasterized at their slot's *device*-pixel size
/// instead — see [`device_pixel_size`] for why.
pub const ICON_SIZE: u32 = 96;

/// Largest size [`render_mesh_icon`] is meant to be asked for. This is
/// an icon rasterizer: the supersampled buffer is `(size · SUPERSAMPLE)²`
/// samples, so an accidental full-viewport request would be a very
/// expensive mistake. Debug builds assert; release clamps nothing, since
/// every caller derives its size from a widget slot.
pub const MAX_ICON_SIZE: u32 = 512;

/// Edge length, in **device** pixels, to rasterize an icon that occupies
/// `logical_side` logical pixels on screen.
///
/// One rule for every icon caller (the favourites strip's slots, the
/// drag ghost): both backends blit with *nearest* sampling, so a render
/// at some other size would point-sample away exactly the supersampled
/// edges this rasterizer just paid for — the render wants to be 1:1 with
/// the pixels it lands on. The scale is agg-gui's `device_scale ·
/// ux_scale`, the same factor `App::layout` divides the viewport by; a
/// changed scale simply asks for a different size, which is a different
/// cache key, so no invalidation hook is needed. Clamped to
/// [`MAX_ICON_SIZE`] as a sanity bound on a nonsense scale factor.
pub fn device_pixel_size(logical_side: f64) -> u32 {
    let scale = agg_gui::ux_scale::effective_scale();
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    ((logical_side * scale).round() as i64).clamp(1, MAX_ICON_SIZE as i64) as u32
}

/// Supersampling factor: the scene is rasterized at `SUPERSAMPLE ×` the
/// requested size and box-filtered down. The ancestor asks WebGL for
/// `antialias: true`; this is our equivalent, and it also gives edge
/// pixels a fractional alpha so an icon composites cleanly over any
/// panel colour. At 96 px it costs 36 864 samples per icon.
pub const SUPERSAMPLE: u32 = 2;

/// Vertical field of view, degrees (`THREE.PerspectiveCamera(30, …)`).
const FOV_DEG: f32 = 30.0;
/// Camera offset direction from the bounding-sphere centre, normalized
/// on use.
const VIEW_DIR: [f32; 3] = [-0.35, -1.0, 0.55];
/// Framing slack on the fitted distance.
const DISTANCE_MARGIN: f32 = 1.15;
/// Ambient term.
const AMBIENT: f32 = 0.55;
/// Key light: world position (direction is `normalize(position)`, three's
/// convention of "shines from here toward the origin") and intensity.
const KEY_LIGHT: ([f32; 3], f32) = ([-40.0, -60.0, 80.0], 1.6);
/// Fill light, same convention.
const FILL_LIGHT: ([f32; 3], f32) = ([60.0, -20.0, 20.0], 0.5);

/// One world-space triangle: three positions, no normals (they are
/// computed per face — see the module docs).
pub type Triangle = [[f32; 3]; 3];

/// Camera distance that frames a bounding sphere of `radius`:
/// NodeDesigner's `radius / tan(fov·π/360) · 1.15`.
///
/// Public so the framing rule can be pinned by a test independently of
/// the rasterizer it feeds — this single number is what makes an icon
/// read at the same scale as its ancestor's.
pub fn fit_distance(radius: f32) -> f32 {
    radius / (FOV_DEG.to_radians() * 0.5).tan() * DISTANCE_MARGIN
}

/// A rendered icon: straight (non-premultiplied) RGBA8, top row first.
///
/// The pixel buffer is an [`Arc`] so repainting every frame re-uses the
/// backend's texture upload: `DrawCtx::draw_image_rgba_arc` keys its
/// cache on the allocation's identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IconImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
}

impl IconImage {
    /// Decoded size in bytes.
    pub fn byte_len(&self) -> usize {
        self.rgba.len()
    }

    /// One pixel, or `None` outside the image. `(0, 0)` is the **top**
    /// left (PNG row order — see the module docs).
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = ((y * self.width + x) * 4) as usize;
        let p = self.rgba.get(i..i + 4)?;
        Some([p[0], p[1], p[2], p[3]])
    }

    /// Fraction of pixels with any coverage — the cheap "did anything
    /// render?" probe the tests and the cache use.
    pub fn coverage(&self) -> f64 {
        if self.rgba.is_empty() {
            return 0.0;
        }
        let covered = self.rgba.chunks_exact(4).filter(|p| p[3] > 0).count();
        covered as f64 / (self.rgba.len() / 4) as f64
    }
}

/// Render `triangles` tinted `color` (RGBA in `0..=1`) into a
/// `size × size` icon.
///
/// `color` is the node's own colour property **as authored** — sRGB
/// components used directly as the lighting term's base, which is what
/// the ancestor does with `ColorManagement` off (see the module docs'
/// colour-space section). It is deliberately *not* converted to linear
/// first; only the lit result is encoded on the way out.
///
/// Returns `None` when there is nothing renderable: no triangles, a
/// zero-extent or non-finite bounding box, or a zero size. Callers treat
/// that as "keep the glyph fallback" — an icon is never a broken image
/// (design §3's rule, applied to the strip).
///
/// The result is **opaque** wherever the mesh covers a pixel (edges get
/// fractional coverage alpha); `color`'s alpha is ignored, so a
/// translucent tint is not supported here — see `shade`.
///
/// `size` must be at most [`MAX_ICON_SIZE`]: this rasterizer is for
/// icons, and a debug build asserts as much rather than quietly
/// allocating a supersampled buffer of arbitrary size.
pub fn render_mesh_icon(triangles: &[Triangle], color: [f32; 4], size: u32) -> Option<IconImage> {
    debug_assert!(
        size <= MAX_ICON_SIZE,
        "render_mesh_icon is an icon path; {size} px exceeds MAX_ICON_SIZE ({MAX_ICON_SIZE})"
    );
    if triangles.is_empty() || size == 0 {
        return None;
    }
    let (center, radius) = bounding_sphere(triangles)?;
    let fov = FOV_DEG.to_radians();
    let distance = fit_distance(radius);
    if !distance.is_finite() || distance <= 0.0 {
        return None;
    }
    let dir = Vec3::from_array(VIEW_DIR).normalize_or_zero();
    if dir == Vec3::ZERO {
        return None;
    }
    let eye = center + dir * distance;
    // Z-up world, matching the 3-D viewport (CLAUDE.md's coordinate note).
    let view = Mat4::look_at_rh(eye, center, Vec3::Z);
    let near = (distance - radius).max(distance * 0.01);
    let far = distance + radius * 2.0;
    let proj = Mat4::perspective_rh_gl(fov, 1.0, near, far);
    let mvp = proj * view;

    let ss = size.saturating_mul(SUPERSAMPLE).max(size);
    let mut buf = SampleBuffer::new(ss);
    for tri in triangles {
        let a = Vec3::from_array(tri[0]);
        let b = Vec3::from_array(tri[1]);
        let c = Vec3::from_array(tri[2]);
        let Some(normal) = face_normal(a, b, c) else {
            continue;
        };
        // Two-sided: face the normal at the eye rather than culling.
        let normal = if normal.dot(eye - a) < 0.0 {
            -normal
        } else {
            normal
        };
        let shaded = shade(color, normal);
        let Some(p) = project_triangle(&mvp, [a, b, c], ss, near) else {
            continue;
        };
        buf.fill_triangle(p, shaded);
    }
    Some(buf.resolve(size))
}

/// Lambert shading: ambient plus the two directional lights, each
/// `intensity · max(dot(N, L), 0)`, applied to the base colour and
/// clamped. Only RGB comes out: transparency in an icon is *coverage*,
/// so `color`'s alpha is deliberately dropped and a lit pixel is always
/// opaque. Translucent tints would need blending in the depth loop and
/// are not supported.
fn shade(color: [f32; 4], normal: Vec3) -> [f32; 3] {
    let mut lambert = AMBIENT;
    for (pos, intensity) in [KEY_LIGHT, FILL_LIGHT] {
        let l = Vec3::from_array(pos).normalize_or_zero();
        lambert += intensity * normal.dot(l).max(0.0);
    }
    [
        (color[0] * lambert).clamp(0.0, 1.0),
        (color[1] * lambert).clamp(0.0, 1.0),
        (color[2] * lambert).clamp(0.0, 1.0),
    ]
}

/// Unit normal of a triangle, or `None` if it is degenerate.
fn face_normal(a: Vec3, b: Vec3, c: Vec3) -> Option<Vec3> {
    let n = (b - a).cross(c - a);
    let len = n.length();
    (len > 1e-12 && len.is_finite()).then(|| n / len)
}

/// Axis-aligned-box centre plus the largest vertex distance from it —
/// the same bounding sphere `THREE.BufferGeometry.computeBoundingSphere`
/// derives, so the framing matches the ancestor's.
fn bounding_sphere(triangles: &[Triangle]) -> Option<(Vec3, f32)> {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for tri in triangles {
        for v in tri {
            let v = Vec3::from_array(*v);
            if !v.is_finite() {
                return None;
            }
            min = min.min(v);
            max = max.max(v);
        }
    }
    if !min.is_finite() || !max.is_finite() {
        return None;
    }
    let center = (min + max) * 0.5;
    let mut radius: f32 = 0.0;
    for tri in triangles {
        for v in tri {
            radius = radius.max((Vec3::from_array(*v) - center).length());
        }
    }
    (radius.is_finite() && radius > 0.0).then_some((center, radius))
}

/// A triangle in raster space: `(x, y, depth)` per vertex, `x`/`y` in
/// pixels with `y` measured **downward** from the top row.
type RasterTriangle = [[f32; 3]; 3];

/// Project a world triangle into raster space, or `None` if any vertex
/// is at/behind the near plane. Whole-triangle rejection is enough here:
/// the camera always sits outside the bounding sphere, so a fitted icon
/// mesh never straddles the near plane.
fn project_triangle(mvp: &Mat4, tri: [Vec3; 3], size: u32, near: f32) -> Option<RasterTriangle> {
    let s = size as f32;
    let mut out = [[0.0f32; 3]; 3];
    for (i, v) in tri.iter().enumerate() {
        let clip = *mvp * v.extend(1.0);
        if clip.w <= near * 1e-3 || !clip.w.is_finite() {
            return None;
        }
        let ndc = clip.xyz() / clip.w;
        out[i] = [
            (ndc.x * 0.5 + 0.5) * s,
            // NDC +Y is up; row 0 is the top, hence the flip.
            (0.5 - ndc.y * 0.5) * s,
            ndc.z,
        ];
    }
    Some(out)
}

/// The supersampled colour + depth target.
struct SampleBuffer {
    size: u32,
    color: Vec<[f32; 3]>,
    depth: Vec<f32>,
    covered: Vec<bool>,
}

impl SampleBuffer {
    fn new(size: u32) -> Self {
        let n = (size as usize) * (size as usize);
        Self {
            size,
            color: vec![[0.0; 3]; n],
            depth: vec![f32::INFINITY; n],
            covered: vec![false; n],
        }
    }

    /// Scanline-free half-space rasterization: bounding box, barycentric
    /// coverage at pixel centres, nearest-depth wins.
    fn fill_triangle(&mut self, tri: RasterTriangle, color: [f32; 3]) {
        let (x0, y0) = (tri[0][0], tri[0][1]);
        let (x1, y1) = (tri[1][0], tri[1][1]);
        let (x2, y2) = (tri[2][0], tri[2][1]);
        let area = (x1 - x0) * (y2 - y0) - (x2 - x0) * (y1 - y0);
        if area.abs() < 1e-9 {
            return;
        }
        let inv_area = 1.0 / area;
        let size = self.size as f32;
        let min_x = x0.min(x1).min(x2).floor().max(0.0) as u32;
        let max_x = x0.max(x1).max(x2).ceil().min(size) as u32;
        let min_y = y0.min(y1).min(y2).floor().max(0.0) as u32;
        let max_y = y0.max(y1).max(y2).ceil().min(size) as u32;
        for py in min_y..max_y {
            for px in min_x..max_x {
                let x = px as f32 + 0.5;
                let y = py as f32 + 0.5;
                // Barycentrics via the same signed-area expression, so
                // the winding sign cancels and either orientation fills.
                let w0 = ((x1 - x) * (y2 - y) - (x2 - x) * (y1 - y)) * inv_area;
                let w1 = ((x2 - x) * (y0 - y) - (x0 - x) * (y2 - y)) * inv_area;
                let w2 = 1.0 - w0 - w1;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                // NDC depth is linear in screen space, so plain
                // barycentric interpolation is exact for the z test.
                let z = w0 * tri[0][2] + w1 * tri[1][2] + w2 * tri[2][2];
                let i = (py * self.size + px) as usize;
                if z < self.depth[i] {
                    self.depth[i] = z;
                    self.color[i] = color;
                    self.covered[i] = true;
                }
            }
        }
    }

    /// Box-filter down to `size`, turning partial coverage into alpha.
    /// Uncovered samples contribute nothing to the colour average, so an
    /// edge pixel is the object's colour at a fractional alpha rather
    /// than the object darkened toward the (transparent) background.
    fn resolve(&self, size: u32) -> IconImage {
        let factor = (self.size / size.max(1)).max(1);
        let mut rgba = vec![0u8; (size as usize) * (size as usize) * 4];
        for y in 0..size {
            for x in 0..size {
                let mut sum = [0.0f32; 3];
                let mut hits = 0u32;
                let mut total = 0u32;
                for sy in 0..factor {
                    for sx in 0..factor {
                        let px = x * factor + sx;
                        let py = y * factor + sy;
                        if px >= self.size || py >= self.size {
                            continue;
                        }
                        total += 1;
                        let i = (py * self.size + px) as usize;
                        if self.covered[i] {
                            hits += 1;
                            sum[0] += self.color[i][0];
                            sum[1] += self.color[i][1];
                            sum[2] += self.color[i][2];
                        }
                    }
                }
                let o = ((y * size + x) * 4) as usize;
                if hits == 0 || total == 0 {
                    continue;
                }
                let inv = 1.0 / hits as f32;
                // Colour channels are encoded on the way out (see the
                // module docs); alpha is coverage, never gamma-encoded.
                rgba[o] = to_srgb_u8(sum[0] * inv);
                rgba[o + 1] = to_srgb_u8(sum[1] * inv);
                rgba[o + 2] = to_srgb_u8(sum[2] * inv);
                rgba[o + 3] = to_u8(hits as f32 / total as f32);
            }
        }
        IconImage {
            width: size,
            height: size,
            rgba: Arc::new(rgba),
        }
    }
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// The sRGB transfer function (IEC 61966-2-1), i.e. exactly what a WebGL
/// renderer with `outputColorSpace = SRGBColorSpace` applies in its
/// output fragment — see the module docs for why the ancestor's icons go
/// through it and ours must too.
pub fn linear_to_srgb(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// One lit colour channel as a stored byte: encode, then quantize.
fn to_srgb_u8(v: f32) -> u8 {
    to_u8(linear_to_srgb(v))
}

#[cfg(test)]
#[path = "mesh_raster_tests.rs"]
mod tests;
