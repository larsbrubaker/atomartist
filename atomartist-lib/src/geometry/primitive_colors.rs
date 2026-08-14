//! Per-primitive default colours, ported from MatterCAD's
//! `Object3DExtensions.PrimitiveColors`.
//!
//! MatterCAD gives every primitive its own hue so a scene of mixed
//! shapes reads at a glance: three anchor colours are fixed, and the
//! rest are HSL blends walked between them. Each primitive node's
//! `properties()` seeds its `color` prop from [`primitive_color`]
//! instead of the generic [`super::DEFAULT_GEOMETRY_COLOR`].
//!
//! The anchors and blend ratios are verbatim from MatterCAD:
//!
//! ```text
//! Cube       HSL(.01, .98, .76)   // red
//! Text       HSL(.175, .98, .76)  // yellow
//! HalfSphere HSL(.87, .98, .76)   // violet
//! Pyramid/Wedge/HalfWedge   = blend(Cube → Text, i/4)     for i in 1..=3
//! Cylinder/Cone/HalfCylinder/Torus/Ring/Sphere
//!                           = blend(Text → HalfSphere, i/7) for i in 1..=6
//! ```

/// Saturation / lightness shared by every MatterCAD primitive colour —
/// the anchors differ only in hue, so the blends are pure hue walks.
const PRIMITIVE_S: f32 = 0.98;
const PRIMITIVE_L: f32 = 0.76;

const HUE_CUBE: f32 = 0.01;
const HUE_TEXT: f32 = 0.175;
const HUE_HALF_SPHERE: f32 = 0.87;

/// Default colour for a primitive, keyed by MatterCAD's primitive name
/// (`"Cube"`, `"Sphere"`, `"Torus"`, …). Unknown names fall back to
/// [`super::DEFAULT_GEOMETRY_COLOR`] so a node type we haven't mapped
/// still gets a sane opaque colour rather than black.
pub fn primitive_color(name: &str) -> [f32; 4] {
    match primitive_hue(name) {
        Some(hue) => {
            let [r, g, b] = hsl_to_rgb(hue, PRIMITIVE_S, PRIMITIVE_L);
            [r, g, b, 1.0]
        }
        None => super::DEFAULT_GEOMETRY_COLOR,
    }
}

/// Hue for a primitive name, or `None` when the name isn't one of
/// MatterCAD's. Split out from [`primitive_color`] so the blend
/// arithmetic can be asserted directly in tests.
fn primitive_hue(name: &str) -> Option<f32> {
    // `BlendHsl(a, b, i, count)` in MatterCAD is
    // `a.BlendHsl(b, 1.0 / (count + 1.0) * i)`. With a shared S and L,
    // blending in HSL reduces to a linear hue interpolation.
    let blend = |a: f32, b: f32, i: f32, count: f32| a + (b - a) * (i / (count + 1.0));
    Some(match name {
        // Anchors.
        "Cube" => HUE_CUBE,
        "Text" => HUE_TEXT,
        "HalfSphere" => HUE_HALF_SPHERE,
        // Cube → Text.
        "Pyramid" => blend(HUE_CUBE, HUE_TEXT, 1.0, 3.0),
        "Wedge" => blend(HUE_CUBE, HUE_TEXT, 2.0, 3.0),
        "HalfWedge" => blend(HUE_CUBE, HUE_TEXT, 3.0, 3.0),
        // Text → HalfSphere.
        "Cylinder" => blend(HUE_TEXT, HUE_HALF_SPHERE, 1.0, 6.0),
        "Cone" => blend(HUE_TEXT, HUE_HALF_SPHERE, 2.0, 6.0),
        "HalfCylinder" => blend(HUE_TEXT, HUE_HALF_SPHERE, 3.0, 6.0),
        "Torus" => blend(HUE_TEXT, HUE_HALF_SPHERE, 4.0, 6.0),
        "Ring" => blend(HUE_TEXT, HUE_HALF_SPHERE, 5.0, 6.0),
        "Sphere" => blend(HUE_TEXT, HUE_HALF_SPHERE, 6.0, 6.0),
        _ => return None,
    })
}

/// HSL → RGB, all components `0..1`. Same formulation as agg-sharp's
/// `ColorF.FromHSL`, which is what MatterCAD's `Color.FromHSL` calls.
fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> [f32; 3] {
    let v = if lightness <= 0.5 {
        lightness * (1.0 + saturation)
    } else {
        lightness + saturation - lightness * saturation
    };
    if v <= 0.0 {
        // Fully dark — agg returns flat grey at `lightness`.
        return [lightness, lightness, lightness];
    }

    let m = lightness + lightness - v;
    let sv = (v - m) / v;
    let h6 = hue.rem_euclid(1.0) * 6.0;
    let sextant = h6.floor() as i32;
    let fract = h6 - sextant as f32;
    let vsf = v * sv * fract;
    let mid1 = m + vsf;
    let mid2 = v - vsf;
    match sextant % 6 {
        0 => [v, mid1, m],
        1 => [mid2, v, m],
        2 => [m, v, mid1],
        3 => [m, mid2, v],
        4 => [mid1, m, v],
        _ => [v, m, mid2],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spot-check against MatterCAD's own table: `Color.FromHSL(.01,
    /// .98, .76)` is the red the Cube ships with. Values computed from
    /// the same HSL formulation agg-sharp uses.
    #[test]
    fn cube_is_matter_cads_red() {
        let c = primitive_color("Cube");
        assert!(c[0] > c[1] && c[1] >= c[2], "cube must read red: {c:?}");
        assert_eq!(c[3], 1.0, "primitive colours are opaque");
        // Hue .01 with S .98 / L .76 → a light, strongly saturated red.
        // v = l + s - l*s = .9952, m = 2l - v = .5248, and hue .01 sits
        // 6% into the red→yellow sextant so green lands at m + vsf.
        assert!((c[0] - 0.9952).abs() < 1e-3, "r = {}", c[0]);
        assert!((c[1] - 0.5530).abs() < 1e-3, "g = {}", c[1]);
        assert!((c[2] - 0.5248).abs() < 1e-3, "b = {}", c[2]);
    }

    /// The blend ratios are the part that's easy to get subtly wrong,
    /// so pin the endpoints and one interior step of each walk.
    #[test]
    fn blend_ratios_match_mattercad() {
        // Cube → Text over 3 steps: 1/4, 2/4, 3/4 of the way.
        let quarter = HUE_CUBE + (HUE_TEXT - HUE_CUBE) * 0.25;
        assert!((primitive_hue("Pyramid").unwrap() - quarter).abs() < 1e-6);
        // Text → HalfSphere over 6 steps: Sphere is 6/7 of the way,
        // NOT all the way to the violet anchor.
        let sphere = HUE_TEXT + (HUE_HALF_SPHERE - HUE_TEXT) * (6.0 / 7.0);
        assert!((primitive_hue("Sphere").unwrap() - sphere).abs() < 1e-6);
        assert!(primitive_hue("Sphere").unwrap() < HUE_HALF_SPHERE);
        // Torus sits between Cone and Ring on the same walk.
        let cone = primitive_hue("Cone").unwrap();
        let torus = primitive_hue("Torus").unwrap();
        let ring = primitive_hue("Ring").unwrap();
        assert!(cone < torus && torus < ring);
    }

    /// Every primitive gets a *distinct* colour — the whole point of
    /// the palette. Compare quantised to 8-bit, which is the precision
    /// the user actually sees.
    #[test]
    fn every_primitive_has_a_distinct_color() {
        let names = [
            "Cube", "Text", "HalfSphere", "Pyramid", "Wedge", "HalfWedge", "Cylinder", "Cone",
            "HalfCylinder", "Torus", "Ring", "Sphere",
        ];
        let mut seen: Vec<[u8; 3]> = Vec::new();
        for name in names {
            let c = primitive_color(name);
            let q = [
                (c[0] * 255.0).round() as u8,
                (c[1] * 255.0).round() as u8,
                (c[2] * 255.0).round() as u8,
            ];
            assert!(!seen.contains(&q), "{name} duplicates an earlier colour");
            seen.push(q);
        }
    }

    #[test]
    fn unknown_name_falls_back_to_the_generic_default() {
        assert_eq!(primitive_color("NotAPrimitive"), super::super::DEFAULT_GEOMETRY_COLOR);
    }

    #[test]
    fn hsl_to_rgb_matches_known_values() {
        // Pure hues at S=1, L=.5 are the primaries.
        assert_eq!(hsl_to_rgb(0.0, 1.0, 0.5), [1.0, 0.0, 0.0]);
        let green = hsl_to_rgb(1.0 / 3.0, 1.0, 0.5);
        assert!((green[1] - 1.0).abs() < 1e-5 && green[0] < 1e-5);
        let blue = hsl_to_rgb(2.0 / 3.0, 1.0, 0.5);
        assert!((blue[2] - 1.0).abs() < 1e-5 && blue[1] < 1e-5);
        // Zero saturation is grey at the lightness.
        assert_eq!(hsl_to_rgb(0.4, 0.0, 0.3), [0.3, 0.3, 0.3]);
    }
}
