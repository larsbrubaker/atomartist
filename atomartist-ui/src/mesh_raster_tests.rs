//! Tests for [`crate::mesh_raster`] — the icon software rasterizer
//! (`docs/file-browser-design.md` §5b, step 6f-2).
//!
//! Assertions are deliberately *structural* (coverage, silhouette,
//! shading variety, determinism) rather than golden images: a golden
//! 96 × 96 PNG would break on any framing tweak and tell us nothing
//! about what actually went wrong.

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

/// A base colour dark enough that shading differences survive the
/// clamp — `GREY` saturates to white under the key light.
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
/// three distinct shades (per-face normals — the ancestor's explicit
/// `toNonIndexed` + `computeVertexNormals`).
#[test]
fn cube_faces_get_distinct_flat_shades() {
    let img = render_cube();
    let mut shades: Vec<u8> = img
        .rgba
        .chunks_exact(4)
        .filter(|p| p[3] == 255)
        .map(|p| p[0])
        .collect();
    shades.sort_unstable();
    shades.dedup();
    assert!(
        shades.len() >= 3,
        "expected three flat face shades, saw {shades:?}"
    );
    // …and only a handful: a smooth-shaded blob would produce dozens.
    assert!(
        shades.len() <= 6,
        "expected flat faces, saw {} distinct shades",
        shades.len()
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
    // A dim base colour: GREY under two lights clamps to white on both
    // orientations, which would hide the very difference under test.
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
            to_u8(near_shade[0]),
            to_u8(near_shade[1]),
            to_u8(near_shade[2])
        ],
        "the centre must wear the NEAR quad's shade, not the far one's"
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
