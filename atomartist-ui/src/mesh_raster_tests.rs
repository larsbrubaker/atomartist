//! Tests for [`crate::mesh_raster`] — the icon software rasterizer
//! (`docs/file-browser-design.md` §5b, step 6f-2).
//!
//! Assertions are deliberately *structural* (coverage, silhouette,
//! shading variety, determinism) rather than golden images: a golden
//! 96 × 96 PNG would break on any framing tweak and tell us nothing
//! about what actually went wrong.

use std::collections::HashMap;

use super::*;

/// A cube spanning `-half ..= half` on every axis, wound
/// counter-clockwise seen from outside (the convention
/// `atomartist_lib::geometry::generate_box` uses).
fn cube(half: f32) -> Vec<Triangle> {
    let h = half;
    let corners = [
        [-h, -h, -h],
        [h, -h, -h],
        [h, h, -h],
        [-h, h, -h],
        [-h, -h, h],
        [h, -h, h],
        [h, h, h],
        [-h, h, h],
    ];
    // Each face as four indices, CCW from outside.
    let faces = [
        [0usize, 3, 2, 1], // -Z
        [4, 5, 6, 7],      // +Z
        [0, 1, 5, 4],      // -Y
        [2, 3, 7, 6],      // +Y
        [1, 2, 6, 5],      // +X
        [0, 4, 7, 3],      // -X
    ];
    let mut tris = Vec::new();
    for f in faces {
        tris.push([corners[f[0]], corners[f[1]], corners[f[2]]]);
        tris.push([corners[f[0]], corners[f[2]], corners[f[3]]]);
    }
    tris
}

const GREY: [f32; 4] = [0.8, 0.8, 0.8, 1.0];

/// The base colour the shade-comparison pins below are written against.
///
/// It used to be here to dodge the clamp — before the Lambert BRDF's
/// `1/π` landed (6h-1), `GREY` reached `0.8 · 1.89 = 1.51` under the key
/// light and saturated to white on several orientations, hiding the very
/// differences those tests assert. Nothing clamps any more: the brightest
/// face of a `GREY` cube is now `0.8 · 0.6014 = 0.48`. `DIM` stays
/// because it keeps those pins in the low mid-tones (linear ≈ 0.13–0.17,
/// encoded ≈ 100–113), where the sRGB curve is steep enough that a small
/// Lambert difference is tens of byte steps apart rather than one or two.
const DIM: [f32; 4] = [0.25, 0.25, 0.25, 1.0];

fn render_cube() -> IconImage {
    render_mesh_icon(&cube(10.0), GREY, ICON_SIZE).expect("a cube renders")
}

/// Same input, same pixels — the property every cache and every pixel
/// assertion in the suite leans on.
#[test]
fn rendering_is_deterministic() {
    let a = render_cube();
    let b = render_cube();
    assert_eq!(a.rgba, b.rgba, "two renders of one mesh must be identical");
}

/// The icon is the requested size and carries straight RGBA8.
#[test]
fn icon_has_the_requested_size() {
    let img = render_cube();
    assert_eq!((img.width, img.height), (ICON_SIZE, ICON_SIZE));
    assert_eq!(img.byte_len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
}

/// Silhouette: a fitted cube covers the middle of the frame and leaves
/// the corners clear, so the icon composites over the strip's chrome.
#[test]
fn cube_silhouette_is_solid_in_the_middle_and_clear_at_the_corners() {
    let img = render_cube();
    let mid = ICON_SIZE / 2;
    let center = img.pixel(mid, mid).expect("centre pixel");
    assert_eq!(center[3], 255, "the middle of the icon must be opaque");
    for (x, y) in [
        (0, 0),
        (ICON_SIZE - 1, 0),
        (0, ICON_SIZE - 1),
        (ICON_SIZE - 1, ICON_SIZE - 1),
    ] {
        let p = img.pixel(x, y).expect("corner pixel");
        assert_eq!(p[3], 0, "corner ({x}, {y}) must stay transparent");
    }
}

/// Framing: NodeDesigner's `distance = r / tan(fov/2) · 1.15` fills a
/// good share of the frame without clipping. Loose bounds — this guards
/// "the camera is somewhere sane", not an exact projection.
#[test]
fn cube_fills_a_reasonable_share_of_the_frame() {
    let coverage = render_cube().coverage();
    assert!(
        (0.15..0.75).contains(&coverage),
        "cube covered {coverage:.3} of the icon; expected a fitted framing"
    );
}

/// Faceted, not smooth: the three visible faces of a cube must land on
/// three distinct *flat* shades (per-face normals — the ancestor's
/// explicit `toNonIndexed` + `computeVertexNormals`).
///
/// Counted by area, not by distinct value: the supersampled resolve
/// blends across the two interior silhouette edges, so a handful of
/// pixels legitimately sit between two face shades. A smooth-shaded blob
/// would instead spread its pixels over dozens of values with no shade
/// owning a meaningful share.
#[test]
fn cube_faces_get_distinct_flat_shades() {
    let img = render_cube();
    let opaque: Vec<u8> = img
        .rgba
        .chunks_exact(4)
        .filter(|p| p[3] == 255)
        .map(|p| p[0])
        .collect();
    assert!(!opaque.is_empty(), "the cube must cover some pixels");
    let mut histogram: HashMap<u8, usize> = HashMap::new();
    for shade in &opaque {
        *histogram.entry(*shade).or_default() += 1;
    }
    // A face of a fitted cube owns roughly a third of the silhouette;
    // 5 % is well above any edge-blend bucket and well below that.
    let floor = opaque.len() / 20;
    let mut major: Vec<u8> = histogram
        .iter()
        .filter(|(_, count)| **count > floor)
        .map(|(shade, _)| *shade)
        .collect();
    major.sort_unstable();
    assert_eq!(
        major.len(),
        3,
        "expected three flat face shades, saw {major:?} of {histogram:?}"
    );
    // …and together they are nearly the whole silhouette.
    let flat: usize = major.iter().map(|s| histogram[s]).sum();
    assert!(
        flat as f64 / opaque.len() as f64 > 0.95,
        "flat faces covered only {flat} of {} opaque pixels",
        opaque.len()
    );
}

/// Two-sided shading: reversing every triangle's winding must not blank
/// the icon (see the module docs on why we do not cull).
#[test]
fn reversed_winding_still_renders_a_solid_object() {
    let flipped: Vec<Triangle> = cube(10.0).into_iter().map(|t| [t[0], t[2], t[1]]).collect();
    let img = render_mesh_icon(&flipped, GREY, ICON_SIZE).expect("flipped cube renders");
    assert!(
        (img.coverage() - render_cube().coverage()).abs() < 1e-6,
        "winding must not change the silhouette"
    );
}

/// Scale invariance: the camera is fitted to the mesh, so a 1 mm cube
/// and a 100 mm cube produce the same picture.
#[test]
fn framing_is_scale_invariant() {
    let small = render_mesh_icon(&cube(0.5), GREY, ICON_SIZE).expect("small cube");
    let large = render_mesh_icon(&cube(50.0), GREY, ICON_SIZE).expect("large cube");
    assert_eq!(small.rgba, large.rgba, "fitted framing must ignore scale");
}

/// The tint reaches the pixels: a red cube is red.
#[test]
fn base_color_drives_the_pixels() {
    let img = render_mesh_icon(&cube(10.0), [0.9, 0.1, 0.1, 1.0], ICON_SIZE).expect("red cube");
    let mid = ICON_SIZE / 2;
    let p = img.pixel(mid, mid).expect("centre pixel");
    assert!(
        p[0] > p[1] && p[0] > p[2],
        "expected a red pixel, got {p:?}"
    );
}

/// Nothing renderable ⇒ no image, so the caller keeps its glyph.
#[test]
fn empty_and_degenerate_input_render_nothing() {
    assert!(
        render_mesh_icon(&[], GREY, ICON_SIZE).is_none(),
        "no triangles"
    );
    let flat: Vec<Triangle> = vec![[[0.0; 3], [0.0; 3], [0.0; 3]]];
    assert!(
        render_mesh_icon(&flat, GREY, ICON_SIZE).is_none(),
        "zero extent"
    );
    assert!(render_mesh_icon(&cube(1.0), GREY, 0).is_none(), "zero size");
    let nan: Vec<Triangle> = vec![[[f32::NAN, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]];
    assert!(
        render_mesh_icon(&nan, GREY, ICON_SIZE).is_none(),
        "non-finite vertex"
    );
}

/// Depth test: a far object hidden behind a near one never paints over
/// it, whatever order the triangles arrive in.
///
/// The two quads are deliberately *differently* oriented, so they take
/// different flat shades and the centre pixel names which one won. With
/// equal orientations the assertion would pass under "last writer wins"
/// too, and would prove nothing.
#[test]
fn nearer_geometry_wins_the_depth_test() {
    let dir = glam::Vec3::from_array(VIEW_DIR).normalize();
    // Tilt the far quad well off the key light so its Lambert term
    // differs from the near one's — but keep it facing the eye, so the
    // renderer's two-sided flip does not change the normal under it.
    let tilted = glam::Vec3::new(0.9, -0.2, 1.0).normalize();
    assert!(tilted.dot(dir) > 0.0, "the far quad must face the camera");
    // `DIM` rather than `GREY` — see its docs. The two orientations
    // shade 0.166 vs 0.127 linear (encoded 113 vs 100), so the centre
    // pixel names which quad won by a comfortable margin.
    let near_shade = shade(DIM, dir);
    let far_shade = shade(DIM, tilted);
    assert!(
        (near_shade[0] - far_shade[0]).abs() > 0.02,
        "the two quads must shade differently for this test to mean anything"
    );

    let near_quad = quad_facing(dir * 5.0, dir, 4.0);
    // Bigger and behind, so it would swamp the near quad if depth were
    // ignored — and drawn *after* it, so draw order favours the wrong one.
    let far_quad = quad_facing(dir * -5.0, tilted, 12.0);
    let mut tris = near_quad;
    tris.extend(far_quad);
    let img = render_mesh_icon(&tris, DIM, ICON_SIZE).expect("quads render");

    let mid = ICON_SIZE / 2;
    let center = img.pixel(mid, mid).expect("centre pixel");
    assert_eq!(center[3], 255, "the near quad must cover the centre");
    assert_eq!(
        [center[0], center[1], center[2]],
        [
            to_srgb_u8(near_shade[0]),
            to_srgb_u8(near_shade[1]),
            to_srgb_u8(near_shade[2])
        ],
        "the centre must wear the NEAR quad's shade, not the far one's"
    );
}

/// The transfer function itself: the IEC 61966-2-1 curve a WebGL
/// renderer with `outputColorSpace = SRGBColorSpace` applies, endpoints
/// and the linear-segment knee included.
#[test]
fn srgb_encode_matches_the_transfer_function() {
    assert!((linear_to_srgb(0.0)).abs() < 1e-6);
    assert!((linear_to_srgb(1.0) - 1.0).abs() < 1e-6);
    // Below the knee the curve is a straight ×12.92.
    assert!((linear_to_srgb(0.002) - 0.025_84).abs() < 1e-6);
    // Mid-grey: linear 0.5 encodes to the familiar ~0.7354 (188/255).
    assert!(
        (linear_to_srgb(0.5) - 0.735_36).abs() < 1e-4,
        "got {}",
        linear_to_srgb(0.5)
    );
    // Monotone, so distinct flat shades stay distinct after encoding.
    assert!(linear_to_srgb(0.4) < linear_to_srgb(0.41));
}

/// Lit pixels are written **through** the encode, the way the ancestor's
/// framebuffer does — not as the raw linear product, which reads darker
/// and more saturated (see the module docs' measured numbers).
#[test]
fn lit_pixels_are_srgb_encoded_not_raw() {
    let dir = Vec3::from_array(VIEW_DIR).normalize();
    // A quad square-on to the camera: its face normal is exactly `dir`,
    // so the expected shade is `shade(DIM, dir)` with no guessing.
    let img = render_mesh_icon(&quad_facing(Vec3::ZERO, dir, 5.0), DIM, ICON_SIZE)
        .expect("the quad renders");
    let mid = ICON_SIZE / 2;
    let p = img.pixel(mid, mid).expect("centre pixel");
    let lit = shade(DIM, dir);
    assert!(
        lit[0] < 1.0,
        "this pin needs an unclamped shade, got {lit:?}"
    );
    assert_eq!(
        [p[0], p[1], p[2]],
        [to_srgb_u8(lit[0]), to_srgb_u8(lit[1]), to_srgb_u8(lit[2])],
        "the written pixel must be the encoded shade"
    );
    // And that is a *visibly* different pixel from the old raw write.
    let raw = to_u8(lit[0]);
    assert!(
        p[0] as i32 - raw as i32 > 30,
        "encoding must lift the midtone: encoded {} vs raw {raw}",
        p[0]
    );
}

/// The exact bytes NodeDesigner's icon renderer produces for a Box, pinned.
///
/// # Derivation (three 0.182, `node_modules/three`)
///
/// `MeshLambertMaterial` is a physical shader in this release: the light
/// uniforms carry `colour · intensity` with no π compensation
/// (`renderers/webgl/WebGLLights.js:263` ambient, `:281` directional —
/// `useLegacyLights` / `physicallyCorrectLights` are gone, and ND sets
/// neither), `getAmbientLightIrradiance` passes the ambient colour
/// straight through (`ShaderChunk/lights_pars_begin.glsl.js:48`), and
/// both `RE_Direct_Lambert` and `RE_IndirectDiffuse_Lambert` multiply
/// their irradiance by `BRDF_Lambert`
/// (`ShaderChunk/lights_lambert_pars_fragment.glsl.js:13-23`), which is
/// `RECIPROCAL_PI · diffuseColor` (`ShaderChunk/common.glsl.js:94`).
/// `ShaderLib/meshlambert.glsl.js:113` sums the two diffuse terms, `:117`
/// tone-maps (no-op — the renderer defaults to `NoToneMapping`) and
/// `:118` applies the sRGB OETF. With `ColorManagement.enabled = false`
/// (`rendering/three-viewer.js:14`) the material colour is the raw sRGB
/// triple. Hence, per face:
///
/// ```text
/// srgb_encode( tint · (0.55 + 1.6·dotNL_key + 0.5·dotNL_fill) / π )
/// ```
///
/// with `key = normalize(-40, -60, 80)` and `fill = normalize(60, -20, 20)`.
/// Evaluated for `#FE8D86` = (254, 141, 134):
///
/// ```text
/// face   lambert   L/π      expected byte triple
/// +X     1.0023   0.3190    (153, 117, 114)
/// -X     1.1442   0.3642    (162, 124, 121)
/// +Y     0.5500   0.1751    (116,  88,  86)   ambient only
/// -Y     1.5921   0.5068    (188, 144, 141)
/// +Z     1.8892   0.6014    (203, 156, 152)
/// -Z     0.5500   0.1751    (116,  88,  86)   ambient only
/// ```
///
/// Before this pin the `1/π` was missing, and the same faces came out
/// (255, 196, 192) / (255, 208, 204) / (195, 150, 146) / (255, 241, 236)
/// / (255, 255, 254) / (195, 150, 146) — a near-white wash instead of the
/// ancestor's reddish-brown box.
#[test]
fn box_faces_match_node_designers_shading_numbers() {
    // MatterCAD's `Cube` tint, #FE8D86, as authored.
    let tint = [254.0 / 255.0, 141.0 / 255.0, 134.0 / 255.0, 1.0];
    let expected: [(Vec3, [u8; 3]); 6] = [
        (Vec3::X, [153, 117, 114]),
        (Vec3::NEG_X, [162, 124, 121]),
        (Vec3::Y, [116, 88, 86]),
        (Vec3::NEG_Y, [188, 144, 141]),
        (Vec3::Z, [203, 156, 152]),
        (Vec3::NEG_Z, [116, 88, 86]),
    ];
    for (normal, want) in expected {
        let lit = shade(tint, normal);
        let got = [to_srgb_u8(lit[0]), to_srgb_u8(lit[1]), to_srgb_u8(lit[2])];
        assert_eq!(got, want, "face {normal:?} shaded {lit:?}");
    }
}

/// The `1/π` itself: the ambient-only face must be exactly
/// `tint · 0.55 / π`, not `tint · 0.55`. Pins the normalization
/// independently of the byte-level table above, so a future lighting
/// tweak cannot quietly drop it.
#[test]
fn every_light_term_passes_through_the_lambert_brdf() {
    // -Z faces away from both lights, so only the ambient term survives.
    let lit = shade([1.0, 1.0, 1.0, 1.0], Vec3::NEG_Z);
    let want = AMBIENT * std::f32::consts::FRAC_1_PI;
    assert!(
        (lit[0] - want).abs() < 1e-6,
        "ambient-only face is {lit:?}, expected {want}"
    );
}

/// A square centred at `center`, facing `normal`, with edge `2 · half`.
fn quad_facing(center: glam::Vec3, normal: glam::Vec3, half: f32) -> Vec<Triangle> {
    let n = normal.normalize();
    let u = if n.z.abs() < 0.9 {
        n.cross(glam::Vec3::Z).normalize()
    } else {
        n.cross(glam::Vec3::X).normalize()
    };
    let v = n.cross(u);
    let a = center - u * half - v * half;
    let b = center + u * half - v * half;
    let c = center + u * half + v * half;
    let d = center - u * half + v * half;
    vec![
        [a.to_array(), b.to_array(), c.to_array()],
        [a.to_array(), c.to_array(), d.to_array()],
    ]
}
