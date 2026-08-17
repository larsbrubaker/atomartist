//! The Boolean node's four operation icons, ported from MatterCAD's
//! hand-authored SVGs (`MatterCAD/StaticData/Icons/{combine, subtract,
//! intersect, subtract_and_replace}.svg`).
//!
//! Registered once at startup into agg-gui's icon registry under the ids
//! `atomartist_lib`'s Boolean node names in `OPERATION_ICONS`; the
//! node's `operation` property then asks for an `EnumIcons` row and the
//! property-row renderer looks the artwork up at paint time. The schema
//! never carries drawings, only ids.
//!
//! ## The artwork's rules (combine.svg's own design note)
//!
//! Everything sits on a 64-unit grid that maps 4:1 onto a 16-px icon:
//! a blue base block from 0 to 52 with a 4-unit outline, and a round
//! operand of radius 22 (inner edge at 16) centred on (42, 42). The
//! operand's band is 6 units rather than the block's 4 on purpose —
//! 4 units is exactly one pixel at 16 px, which rasterises crisply on
//! the block's axis-aligned edges but antialiases to a broken smudge
//! around the arc; 6 units always covers a whole pixel wherever the
//! curve lands. Arcs are chopped into ≤30° pieces so that flatteners
//! which subdivide per segment still produce a smooth curve.
//!
//! ## Colour roles — the part that carries meaning
//!
//! MatterCAD recolours icons through `GrayToColor`, which rewrites any
//! colour with HSL saturation below ~0.01 to the theme's text colour and
//! leaves saturated colours alone. The artwork exploits that:
//!
//! - `#4C4C4C` **ink** is chrome (outlines, the minus sign) and follows
//!   the theme → [`IconColor::Ink`].
//! - `#9A9A9D` **removed material** reads as a neutral grey but sits at
//!   saturation ~0.015, just above the cutoff, so it passes through both
//!   themes unchanged. That is deliberate: this grey encodes state
//!   ("this material is discarded"), and if it followed the theme it
//!   would go near-black in the light theme and become indistinguishable
//!   from the ink. It is therefore a **literal**, not ink.
//! - `#4BA9E8` **kept material** and `#F20D0D` **the retained remover**
//!   of Subtract & Replace are saturated for the same reason → literals.
//!
//! The path data below is copied verbatim from the SVGs so the two stay
//! comparable by eye; the parser in `agg_gui::vector_icon` accepts the
//! `M/H/V/L/A/Z` subset they use.

use agg_gui::draw_ctx::FillRule;
use agg_gui::vector_icon::{register_svg_icon, IconColor, IconPathError};
use agg_gui::Color;

use atomartist_lib::nodes::ops_3d::boolean_node::OPERATION_ICONS;

/// Kept material — the blue solid that survives the operation.
const KEPT: IconColor = IconColor::Literal(Color::from_rgb8(0x4B, 0xA9, 0xE8));
/// Removed material — the grey that must NOT follow the theme.
const REMOVED: IconColor = IconColor::Literal(Color::from_rgb8(0x9A, 0x9A, 0x9D));
/// The remover kept as its own part by Subtract & Replace.
const REPLACED: IconColor = IconColor::Literal(Color::from_rgb8(0xF2, 0x0D, 0x0D));

/// The operand ring: a full disc contour followed by its inner circle,
/// filled even-odd so the ring is hollow. Shared by three of the four
/// icons, verbatim from the SVGs.
const OPERAND_RING: &str =
    "M64 42 A22 22 0 0 1 61.053 53 A22 22 0 0 1 53 61.053 A22 22 0 0 1 42 64 \
     A22 22 0 0 1 31 61.053 A22 22 0 0 1 22.947 53 A22 22 0 0 1 20 42 A22 22 0 0 1 22.947 31 \
     A22 22 0 0 1 31 22.947 A22 22 0 0 1 42 20 A22 22 0 0 1 53 22.947 A22 22 0 0 1 61.053 31 \
     A22 22 0 0 1 64 42 Z M58 42 A16 16 0 0 1 55.856 50 A16 16 0 0 1 50 55.856 A16 16 0 0 1 42 58 \
     A16 16 0 0 1 34 55.856 A16 16 0 0 1 28.144 50 A16 16 0 0 1 26 42 A16 16 0 0 1 28.144 34 \
     A16 16 0 0 1 34 28.144 A16 16 0 0 1 42 26 A16 16 0 0 1 50 28.144 A16 16 0 0 1 55.856 34 \
     A16 16 0 0 1 58 42 Z";

/// The disc's full interior, painted under the ring so the two never
/// leave an antialiased seam between them.
const OPERAND_DISC: &str =
    "M64 42 A22 22 0 0 1 61.053 53 A22 22 0 0 1 53 61.053 A22 22 0 0 1 42 64 \
     A22 22 0 0 1 31 61.053 A22 22 0 0 1 22.947 53 A22 22 0 0 1 20 42 A22 22 0 0 1 22.947 31 \
     A22 22 0 0 1 31 22.947 A22 22 0 0 1 42 20 A22 22 0 0 1 53 22.947 A22 22 0 0 1 61.053 31 \
     A22 22 0 0 1 64 42 Z";

/// The lens where the block and the operand's inner edge overlap — the
/// volume Intersect keeps and Subtract & Replace hands back as its own
/// part.
const OVERLAP_LENS: &str = "M48 27.168 A16 16 0 0 0 40.665 26.056 A16 16 0 0 0 33.617 28.372 \
     A16 16 0 0 0 28.372 33.617 A16 16 0 0 0 26.056 40.665 A16 16 0 0 0 27.168 48 H48 Z";

/// Combine (union): both solids are kept, so nothing is discarded —
/// there is no removed-material grey here, the outline follows the
/// merged silhouette and the whole blob is one piece of blue material.
fn combine() -> Result<(), IconPathError> {
    register_svg_icon(
        OPERATION_ICONS[0],
        64.0,
        &[
            (
                "M0 0 H52 V22.404 A22 22 0 0 1 59.804 29.077 A22 22 0 0 1 63.73 38.565 \
                 A22 22 0 0 1 62.922 48.802 A22 22 0 0 1 57.556 57.556 A22 22 0 0 1 48.802 62.922 \
                 A22 22 0 0 1 38.565 63.73 A22 22 0 0 1 29.077 59.804 A22 22 0 0 1 22.404 52 H0 Z",
                IconColor::Ink,
                FillRule::NonZero,
            ),
            (
                "M4 4 H48 V27.168 A16 16 0 0 1 54.305 31.773 A16 16 0 0 1 57.68 38.813 \
                 A16 16 0 0 1 57.321 46.613 A16 16 0 0 1 53.314 53.314 A16 16 0 0 1 46.613 57.321 \
                 A16 16 0 0 1 38.813 57.68 A16 16 0 0 1 31.773 54.305 A16 16 0 0 1 27.168 48 H4 Z",
                KEPT,
                FillRule::NonZero,
            ),
        ],
    )
}

/// Subtract: the round operand takes a bite out of the blue base solid
/// and is drawn as a solid tool carrying a minus sign. The tool is
/// removed material, so its interior is the grey and only its ring is
/// themed ink; the minus is painted straight on top in ink, which
/// contrasts with the grey in either theme.
fn subtract() -> Result<(), IconPathError> {
    register_svg_icon(
        OPERATION_ICONS[1],
        64.0,
        &[
            (
                "M0 0 H52 V22.404 A22 22 0 0 0 41.317 20.011 A22 22 0 0 0 30.803 23.062 \
                 A22 22 0 0 0 23.062 30.803 A22 22 0 0 0 20.011 41.317 A22 22 0 0 0 22.404 52 H0 Z",
                IconColor::Ink,
                FillRule::NonZero,
            ),
            (
                "M4 4 H48 V20.834 A22 22 0 0 0 38.747 20.242 A22 22 0 0 0 30.071 23.515 \
                 A22 22 0 0 0 23.515 30.071 A22 22 0 0 0 20.242 38.747 A22 22 0 0 0 20.834 48 H4 Z",
                KEPT,
                FillRule::NonZero,
            ),
            (OPERAND_DISC, REMOVED, FillRule::NonZero),
            (OPERAND_RING, IconColor::Ink, FillRule::EvenOdd),
            ("M32 40 H52 V44 H32 Z", IconColor::Ink, FillRule::NonZero),
        ],
    )
}

/// Intersect: only the overlap survives. Everything else is discarded,
/// so the base block and the part of the operand outside it are both
/// filled with the removed-material grey — as one continuous region,
/// with the disc deliberately covering the block's ink edge where the
/// two meet — and only the lens stays blue.
fn intersect() -> Result<(), IconPathError> {
    register_svg_icon(
        OPERATION_ICONS[2],
        64.0,
        &[
            ("M0 0 H52 V52 H0 Z", IconColor::Ink, FillRule::NonZero),
            ("M4 4 H48 V48 H4 Z", REMOVED, FillRule::NonZero),
            (
                "M58 42 A16 16 0 0 1 55.856 50 A16 16 0 0 1 50 55.856 A16 16 0 0 1 42 58 \
                 A16 16 0 0 1 34 55.856 A16 16 0 0 1 28.144 50 A16 16 0 0 1 26 42 \
                 A16 16 0 0 1 28.144 34 A16 16 0 0 1 34 28.144 A16 16 0 0 1 42 26 \
                 A16 16 0 0 1 50 28.144 A16 16 0 0 1 55.856 34 A16 16 0 0 1 58 42 Z",
                REMOVED,
                FillRule::NonZero,
            ),
            (OPERAND_RING, IconColor::Ink, FillRule::EvenOdd),
            (OVERLAP_LENS, KEPT, FillRule::NonZero),
        ],
    )
}

/// Subtract & Replace: the same bite as Subtract, but the removed chunk
/// is kept as the replacement solid, so it is painted red instead of cut
/// away. The discarded rest of the operand stops at the block's outer
/// edge rather than flowing across it the way Intersect's does, because
/// the block is kept material and its outline has to stay unbroken.
fn subtract_and_replace() -> Result<(), IconPathError> {
    register_svg_icon(
        OPERATION_ICONS[3],
        64.0,
        &[
            ("M0 0 H52 V52 H0 Z", IconColor::Ink, FillRule::NonZero),
            (
                "M4 4 H48 V20.834 A22 22 0 0 0 38.747 20.242 A22 22 0 0 0 30.071 23.515 \
                 A22 22 0 0 0 23.515 30.071 A22 22 0 0 0 20.242 38.747 A22 22 0 0 0 20.834 48 H4 Z",
                KEPT,
                FillRule::NonZero,
            ),
            (
                "M52 29.51 A16 16 0 0 1 56.64 35.544 A16 16 0 0 1 57.966 43.038 \
                 A16 16 0 0 1 55.68 50.298 A16 16 0 0 1 50.298 55.68 A16 16 0 0 1 43.038 57.966 \
                 A16 16 0 0 1 35.544 56.64 A16 16 0 0 1 29.51 52 L52 52 Z",
                REMOVED,
                FillRule::NonZero,
            ),
            (OPERAND_RING, IconColor::Ink, FillRule::EvenOdd),
            (OVERLAP_LENS, REPLACED, FillRule::NonZero),
        ],
    )
}

/// Register all four Boolean operation icons. Idempotent — a second
/// call simply replaces the registrations.
///
/// Called from `shell_init::install_theme_and_fonts` and nowhere else:
/// both shells and the UI-test harness run that one startup function, so
/// there is a single registration site and a test can prove the shipped
/// app performs it.
///
/// The path data is a compile-time constant, so a parse failure is a
/// programming error rather than a runtime condition; it is reported
/// through a `Result` anyway so no icon can vanish silently and the
/// tests can assert on it.
pub(crate) fn register_boolean_icons() -> Result<(), IconPathError> {
    combine()?;
    subtract()?;
    intersect()?;
    subtract_and_replace()
}

// Where the artwork lands on screen (the Y-down → Y-up flip) is checked
// separately, because it needs a recording `DrawCtx`.
#[cfg(test)]
#[path = "boolean_icons_tests.rs"]
mod orientation_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use agg_gui::vector_icon::icon;

    /// All four ids resolve to real artwork after registration, and each
    /// icon fills the 64-unit box the way the family's geometry says it
    /// should.
    #[test]
    fn every_operation_icon_registers_with_art_in_it() {
        register_boolean_icons().expect("the bundled path data parses");
        for id in OPERATION_ICONS {
            let art = icon(id).unwrap_or_else(|| panic!("no icon registered for {id}"));
            assert!(!art.paths.is_empty(), "{id} has no paths");
            assert!(art.point_count() > 20, "{id} has almost no geometry");
            let b = art.bounds().unwrap_or_else(|| panic!("{id} has no bounds"));
            assert!(
                b[0] >= -0.01 && b[1] >= -0.01,
                "{id} starts outside the box"
            );
            assert!(
                b[2] <= 64.01 && b[3] <= 64.01,
                "{id} overflows the 64-unit box: {b:?}"
            );
            // Every icon in the family spans most of the grid — the
            // block reaches 52 and the operand reaches 64.
            assert!(
                b[2] - b[0] > 50.0 && b[3] - b[1] > 50.0,
                "{id} is tiny: {b:?}"
            );
        }
    }

    /// The four icons are actually different drawings — a copy-paste
    /// slip in the path data would otherwise ship four Combines.
    #[test]
    fn the_four_icons_are_distinct() {
        register_boolean_icons().expect("the bundled path data parses");
        let arts: Vec<_> = OPERATION_ICONS
            .iter()
            .map(|id| icon(id).unwrap_or_else(|| panic!("no icon for {id}")))
            .collect();
        for (i, a) in arts.iter().enumerate() {
            for (j, b) in arts.iter().enumerate().skip(i + 1) {
                assert_ne!(
                    a.paths, b.paths,
                    "{} and {} are the same drawing",
                    OPERATION_ICONS[i], OPERATION_ICONS[j]
                );
            }
        }
    }

    /// The colour roles are the whole design: the ink follows the theme,
    /// the state colours must not. Combine has no removed material;
    /// only Subtract & Replace carries red.
    #[test]
    fn colour_roles_match_the_artworks_intent() {
        register_boolean_icons().expect("the bundled path data parses");
        let roles = |id: &str| -> Vec<IconColor> {
            icon(id)
                .unwrap_or_else(|| panic!("no icon for {id}"))
                .paths
                .iter()
                .map(|p| p.fill)
                .collect()
        };
        for id in OPERATION_ICONS {
            assert!(
                roles(id).contains(&IconColor::Ink),
                "{id} has no themed linework"
            );
        }
        assert!(
            !roles(OPERATION_ICONS[0]).contains(&REMOVED),
            "Combine discards nothing, so it must carry no removed-material grey"
        );
        for id in &OPERATION_ICONS[1..] {
            assert!(
                roles(id).contains(&REMOVED),
                "{id} discards material but paints no removed-material grey"
            );
        }
        assert!(
            roles(OPERATION_ICONS[3]).contains(&REPLACED),
            "Subtract & Replace must paint the retained remover red"
        );
        for id in &OPERATION_ICONS[..3] {
            assert!(
                !roles(id).contains(&REPLACED),
                "{id} must not use the retained-remover red"
            );
        }
    }

    /// The operand ring is an even-odd path — a hollow ring, not a solid
    /// disc. Filling it non-zero would blot out the material inside it.
    #[test]
    fn the_operand_ring_is_filled_even_odd() {
        register_boolean_icons().expect("the bundled path data parses");
        for id in &OPERATION_ICONS[1..] {
            let art = icon(id).unwrap_or_else(|| panic!("no icon for {id}"));
            let ring = art
                .paths
                .iter()
                .find(|p| p.fill_rule == FillRule::EvenOdd)
                .unwrap_or_else(|| panic!("{id} has no even-odd ring"));
            assert_eq!(
                ring.contours.len(),
                2,
                "{id}'s ring needs an outer and an inner contour"
            );
        }
    }
}
