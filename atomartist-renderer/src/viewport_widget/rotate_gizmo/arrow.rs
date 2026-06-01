//! Rotation-arrow icon geometry for the rotate gizmo's handle plate.
//!
//! Port of MatterCAD `RotateCornerControl`'s `Arrows` vector art — the
//! curved double-arrow glyph it renders into a 64×64 texture and places
//! on the rotate handle's faces. Where MatterCAD bakes the path into an
//! `ImageBuffer` and texture-maps it, AtomArtist's gizmo pass draws flat
//! `GizmoTriangleSet`s, so we instead **triangulate** the glyph once
//! (via the same `tess2-rust` the Extrude node uses) and the handle maps
//! the resulting 2-D triangle soup into its in-plane `(u, v)` basis — see
//! [`super::handle`].
//!
//! The triangle soup is built lazily on first use and cached: parsing +
//! flattening + tessellating the fixed glyph costs nothing per frame.
//! Output is centred on the origin and normalised so the glyph's larger
//! dimension spans `1.0` (i.e. every vertex lies in `[-0.5, 0.5]`), with
//! SVG's Y-down flipped to the viewport's Y-up. The handle then scales by
//! `handle_size`, matching the old square plate's footprint.

use std::sync::OnceLock;

use tess2_rust::{ElementType, Tessellator, WindingRule};

/// MatterCAD's `RotateCornerControl.Arrows` path data, verbatim — the
/// curved rotate-arrow glyph. Absolute commands only (`M`/`L`/`C`/`z`).
const ROTATE_ARROW_PATH: &str = "M267.96599,177.26875L276.43374,168.80101C276.43374,170.2123 276.43374,171.62359 276.43374,173.03488C280.02731,173.01874 282.82991,174.13254 286.53647,171.29154C290.08503,168.16609 288.97661,164.24968 289.13534,160.33327L284.90147,160.33327L293.36921,151.86553L301.83695,160.33327L297.60308,160.33327C297.60308,167.38972 298.67653,171.4841 293.23666,177.24919C286.80975,182.82626 283.014,181.02643 276.43374,181.50262L276.43374,185.73649L267.96599,177.26875L267.96599,177.26875z";

/// Subdivisions per cubic Bézier segment when flattening the glyph.
/// 16 keeps the curved arrow bodies smooth at the handle's on-screen
/// size without bloating the triangle count.
const CUBIC_SEGMENTS: usize = 16;

/// Angle (radians, measured in the glyph's own 2-D frame from +X toward
/// +Y) that the double-arrow's *opening* faces in its natural,
/// unrotated orientation. The two arrowhead tips sit at roughly
/// `OPENING ± 71°` (their bisector is the opening direction), so to make
/// a handle's arrowheads point along the two box edges at its corner,
/// rotate the glyph so this opening faces the corner's inward diagonal
/// (toward the box centre). Derived from the glyph geometry: the tips
/// land at ≈63° and ≈207° → bisector ≈135°. Tunable if the curl ever
/// needs to flip — adding `PI` faces the opening the other way.
pub const GLYPH_OPENING_ANGLE: f32 = 3.0 * std::f32::consts::FRAC_PI_4;

/// Triangle soup (`[x, y]` triples, one winding) for the rotate-arrow
/// glyph, centred on the origin and normalised to `[-0.5, 0.5]`.
/// Cached after the first call. Falls back to a unit square if the glyph
/// ever fails to tessellate so the handle is never invisible.
pub fn arrow_icon_triangles() -> &'static [[f32; 2]] {
    static CACHE: OnceLock<Vec<[f32; 2]>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let contour = parse_path_to_contour(ROTATE_ARROW_PATH);
        let tris = tessellate_contour(&contour);
        if tris.len() >= 3 {
            normalize(tris)
        } else {
            unit_square()
        }
    })
}

/// A centred unit square (two triangles) — the pre-arrow placeholder
/// shape, reused as the tessellation fallback.
fn unit_square() -> Vec<[f32; 2]> {
    let a = [-0.5, -0.5];
    let b = [0.5, -0.5];
    let c = [0.5, 0.5];
    let d = [-0.5, 0.5];
    vec![a, b, c, a, c, d]
}

/// One token of SVG path data: a command letter or a number.
enum Tok {
    Cmd(char),
    Num(f64),
}

/// Split path data into command/number tokens. Handles the comma- and
/// space-separated, letter-delimited form MatterCAD emits (numbers can
/// abut the next command letter with no separator).
fn tokenize(s: &str) -> Vec<Tok> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_alphabetic() {
            out.push(Tok::Cmd(c));
            i += 1;
        } else if c == ',' || c.is_whitespace() {
            i += 1;
        } else if c == '-' || c == '+' || c == '.' || c.is_ascii_digit() {
            let start = i;
            i += 1;
            while i < bytes.len() {
                let d = bytes[i] as char;
                let prev = bytes[i - 1] as char;
                if d.is_ascii_digit() || d == '.' {
                    i += 1;
                } else if (d == 'e' || d == 'E') && prev.is_ascii_digit() {
                    i += 1;
                } else if (d == '-' || d == '+') && (prev == 'e' || prev == 'E') {
                    i += 1;
                } else {
                    break;
                }
            }
            if let Ok(n) = s[start..i].parse::<f64>() {
                out.push(Tok::Num(n));
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Parse absolute `M`/`L`/`C`/`Z` path data into a single closed contour
/// of points, flattening each cubic into [`CUBIC_SEGMENTS`] line steps.
/// Lowercase/relative commands and other glyph features aren't needed
/// for the fixed rotate-arrow path, so they're not handled.
fn parse_path_to_contour(s: &str) -> Vec<[f64; 2]> {
    let toks = tokenize(s);
    let mut pts: Vec<[f64; 2]> = Vec::new();
    let mut cur = [0.0_f64, 0.0];
    let mut start = [0.0_f64, 0.0];
    let mut cmd = ' ';
    let mut t = 0usize;

    // Pull the next number token, skipping any stray commands.
    let next_num = |toks: &[Tok], t: &mut usize| -> Option<f64> {
        while *t < toks.len() {
            if let Tok::Num(n) = toks[*t] {
                *t += 1;
                return Some(n);
            }
            *t += 1;
        }
        None
    };

    while t < toks.len() {
        // A command token switches the active command; otherwise the
        // previous command repeats (SVG implicit repetition).
        if let Tok::Cmd(c) = toks[t] {
            cmd = c;
            t += 1;
        }
        match cmd {
            'M' => {
                let (Some(x), Some(y)) = (next_num(&toks, &mut t), next_num(&toks, &mut t)) else { break };
                cur = [x, y];
                start = cur;
                pts.push(cur);
                // Per SVG, coordinate pairs after the initial moveto are
                // implicit lineto.
                cmd = 'L';
            }
            'L' => {
                let (Some(x), Some(y)) = (next_num(&toks, &mut t), next_num(&toks, &mut t)) else { break };
                cur = [x, y];
                pts.push(cur);
            }
            'C' => {
                let nums = [
                    next_num(&toks, &mut t),
                    next_num(&toks, &mut t),
                    next_num(&toks, &mut t),
                    next_num(&toks, &mut t),
                    next_num(&toks, &mut t),
                    next_num(&toks, &mut t),
                ];
                let [Some(c1x), Some(c1y), Some(c2x), Some(c2y), Some(ex), Some(ey)] = nums else {
                    break;
                };
                flatten_cubic(cur, [c1x, c1y], [c2x, c2y], [ex, ey], &mut pts);
                cur = [ex, ey];
            }
            'Z' | 'z' => {
                // Close back to the subpath start. The tessellator treats
                // the contour as implicitly closed, so this is mostly a
                // no-op, but keep `cur` consistent.
                cur = start;
                t += 1;
            }
            _ => {
                t += 1;
            }
        }
    }
    pts
}

/// Append the flattened interior + end points of one cubic Bézier
/// (`p0` is already the last point in the contour, so it's skipped).
fn flatten_cubic(p0: [f64; 2], p1: [f64; 2], p2: [f64; 2], p3: [f64; 2], out: &mut Vec<[f64; 2]>) {
    for i in 1..=CUBIC_SEGMENTS {
        let t = i as f64 / CUBIC_SEGMENTS as f64;
        let mt = 1.0 - t;
        let a = mt * mt * mt;
        let b = 3.0 * mt * mt * t;
        let c = 3.0 * mt * t * t;
        let d = t * t * t;
        out.push([
            a * p0[0] + b * p1[0] + c * p2[0] + d * p3[0],
            a * p0[1] + b * p1[1] + c * p2[1] + d * p3[1],
        ]);
    }
}

/// Tessellate a closed 2-D contour into a flat triangle soup (`[x, y]`
/// per vertex, 3 per triangle). Empty on failure.
fn tessellate_contour(contour: &[[f64; 2]]) -> Vec<[f32; 2]> {
    if contour.len() < 3 {
        return Vec::new();
    }
    let mut flat: Vec<f64> = Vec::with_capacity(contour.len() * 2);
    for p in contour {
        flat.push(p[0]);
        flat.push(p[1]);
    }
    let mut tess = Tessellator::new();
    tess.add_contour(2, &flat);
    let ok = tess.tessellate(
        WindingRule::NonZero,
        ElementType::Polygons,
        3,
        2,
        Some([0.0, 0.0, 1.0]),
    );
    if !ok {
        return Vec::new();
    }
    let verts = tess.vertices();
    let elems = tess.elements();
    let mut tris = Vec::with_capacity(elems.len());
    for tri in elems.chunks_exact(3) {
        if tri.iter().any(|&v| v == u32::MAX) {
            continue;
        }
        for &vi in tri {
            let vi = vi as usize;
            if vi * 2 + 1 < verts.len() {
                tris.push([verts[vi * 2] as f32, verts[vi * 2 + 1] as f32]);
            }
        }
    }
    tris
}

/// Centre a triangle soup on the origin and scale so its larger
/// dimension spans `1.0` (vertices land in `[-0.5, 0.5]`). SVG's Y-down
/// is flipped to the viewport's Y-up so the glyph reads upright.
fn normalize(tris: Vec<[f32; 2]>) -> Vec<[f32; 2]> {
    let mut mn = [f32::INFINITY; 2];
    let mut mx = [f32::NEG_INFINITY; 2];
    for p in &tris {
        for k in 0..2 {
            mn[k] = mn[k].min(p[k]);
            mx[k] = mx[k].max(p[k]);
        }
    }
    let cx = (mn[0] + mx[0]) * 0.5;
    let cy = (mn[1] + mx[1]) * 0.5;
    let extent = (mx[0] - mn[0]).max(mx[1] - mn[1]).max(1e-6);
    let scale = 1.0 / extent;
    tris.into_iter()
        .map(|p| [(p[0] - cx) * scale, -(p[1] - cy) * scale])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_parses_to_a_closed_contour() {
        let contour = parse_path_to_contour(ROTATE_ARROW_PATH);
        // Each of the 4 cubics contributes CUBIC_SEGMENTS points plus the
        // straight moves — plenty of points, and the loop should return
        // near its start.
        assert!(contour.len() > 20, "expected a flattened contour, got {}", contour.len());
        let first = contour[0];
        let last = *contour.last().unwrap();
        let d = ((first[0] - last[0]).powi(2) + (first[1] - last[1]).powi(2)).sqrt();
        assert!(d < 1.0, "contour should return near its start, gap = {d}");
    }

    #[test]
    fn arrow_triangles_are_nonempty_normalised_triples() {
        let tris = arrow_icon_triangles();
        assert!(tris.len() >= 3, "arrow must tessellate to at least one triangle");
        assert_eq!(tris.len() % 3, 0, "triangle soup must be whole triangles");
        // It must NOT be the bare fallback square (6 verts) — that would
        // mean tessellation silently failed and we regressed to a plate.
        assert!(tris.len() > 6, "expected the arrow glyph, not the square fallback");
        for v in tris {
            assert!(v[0] >= -0.5001 && v[0] <= 0.5001, "x out of normalised range: {}", v[0]);
            assert!(v[1] >= -0.5001 && v[1] <= 0.5001, "y out of normalised range: {}", v[1]);
        }
    }
}
