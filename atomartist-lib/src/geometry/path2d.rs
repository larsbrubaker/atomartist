//! 2D path utilities — re-exports `manifold_rust::cross_section::CrossSection`
//! plus thin helpers that callers in AtomArtist need without reaching into
//! `manifold-rust` directly.
//!
//! `CrossSection` already provides Boolean operations (`union`,
//! `intersection`, `difference`), `offset`, `area`, and `bounds`. Use those
//! directly. The helpers here exist for things that aren't on `CrossSection`
//! itself — like extracting contours in a form ready for tessellation.

pub use manifold_rust::cross_section::CrossSection;
pub use manifold_rust::linalg::Vec2 as Vec2D;

/// Signed area of a single polygon contour. Positive for CCW, negative for
/// CW (the standard "shoelace" formula). Used to distinguish outer
/// boundaries from holes when tessellating extrudes.
pub fn signed_area(contour: &[Vec2D]) -> f64 {
    if contour.len() < 3 {
        return 0.0;
    }
    let mut a = 0.0;
    for i in 0..contour.len() {
        let p0 = contour[i];
        let p1 = contour[(i + 1) % contour.len()];
        a += p0.x * p1.y - p1.x * p0.y;
    }
    a * 0.5
}

/// Returns true if the contour is wound counter-clockwise.
pub fn is_ccw(contour: &[Vec2D]) -> bool {
    signed_area(contour) > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sq() -> Vec<Vec2D> {
        vec![
            Vec2D::new(0.0, 0.0),
            Vec2D::new(1.0, 0.0),
            Vec2D::new(1.0, 1.0),
            Vec2D::new(0.0, 1.0),
        ]
    }

    #[test]
    fn signed_area_unit_square_ccw_is_one() {
        let s = sq();
        assert!((signed_area(&s) - 1.0).abs() < 1e-9);
        assert!(is_ccw(&s));
    }

    #[test]
    fn signed_area_unit_square_cw_is_negative_one() {
        let mut s = sq();
        s.reverse();
        assert!((signed_area(&s) + 1.0).abs() < 1e-9);
        assert!(!is_ccw(&s));
    }

    #[test]
    fn cross_section_union_round_trip() {
        let a = CrossSection::square(2.0);
        let b = CrossSection::circle(1.0, 32);
        let _ = a.union(&b);
    }
}
