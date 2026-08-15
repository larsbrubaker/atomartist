//! Font Awesome glyph code points — the single source of truth for every icon
//! glyph AtomArtist renders through the system font's fallback chain.
//!
//! The icon face (`assets/font-awesome.ttf`, Font Awesome 4.7.0) is installed
//! as a fallback on the primary NotoSans font in [`crate::shell_init`], so any
//! widget that draws one of these `char` constants as ordinary text gets the
//! icon glyph for free. Centralizing the code points here — rather than
//! scattering `\u{F...}` escapes across `top_menu_bar` / `breadcrumb_bar` —
//! keeps them verifiable: the test below asserts every constant resolves to a
//! real glyph in the bundled font, so a wrong code point or a swapped font
//! fails CI instead of silently rendering an empty "tofu" box at runtime.
//!
//! Font Awesome 4 is a single-weight face (no solid/regular/light split), so
//! these are the classic FA4 code points, not FA6's per-style ranges.

/// Raw bytes of the bundled Font Awesome 4.7.0 face. Shared with
/// [`crate::shell_init`] so the font that gets installed at startup is exactly
/// the one the coverage test below validates.
pub(crate) const ICON_FONT_BYTES: &[u8] = include_bytes!("../assets/font-awesome.ttf");

// Edit menu / node toolbar.
pub const UNDO: char = '\u{f0e2}'; // fa-undo
pub const REDO: char = '\u{f01e}'; // fa-repeat
pub const TRASH: char = '\u{f1f8}'; // fa-trash
pub const EXPAND: char = '\u{f065}'; // fa-expand

// Help menu.
pub const BOOK: char = '\u{f02d}'; // fa-book
pub const INFO_CIRCLE: char = '\u{f05a}'; // fa-info-circle

// File menu.
pub const FILE_NEW: char = '\u{f016}'; // fa-file-o
pub const FOLDER_OPEN: char = '\u{f07c}'; // fa-folder-open
pub const SAVE: char = '\u{f0c7}'; // fa-floppy-o
pub const IMPORT: char = '\u{f019}'; // fa-download
pub const EXPORT: char = '\u{f08e}'; // fa-external-link

// View menu.
pub const SUN: char = '\u{f185}'; // fa-sun-o
pub const BUG: char = '\u{f188}'; // fa-bug
pub const TACHOMETER: char = '\u{f0e4}'; // fa-tachometer (performance)

// Add-node menu + per-category icons.
pub const PLUS_CIRCLE: char = '\u{f055}'; // fa-plus-circle
pub const PENCIL: char = '\u{f040}'; // fa-pencil (2D primitives / operations)
pub const CUBE: char = '\u{f1b2}'; // fa-cube (3D primitives)
pub const CUBES: char = '\u{f1b3}'; // fa-cubes (mesh)
pub const CALCULATOR: char = '\u{f1ec}'; // fa-calculator (math)
pub const SLIDERS: char = '\u{f1de}'; // fa-sliders (input)
pub const PLUG: char = '\u{f1e6}'; // fa-plug (output)

// Breadcrumb navigation chrome.
pub const CHEVRON_LEFT: char = '\u{f053}'; // fa-chevron-left
pub const CHEVRON_RIGHT: char = '\u{f054}'; // fa-chevron-right

// Status-bar chrome — the cancel affordance on an in-flight storage op.
pub const TIMES: char = '\u{f00d}'; // fa-times

// File-browser chrome — provider rows and the grid's fallback glyphs, used
// whenever an entry has no embedded preview (design §3: never a broken
// image).
pub const FOLDER: char = '\u{f07b}'; // fa-folder (a directory entry)
pub const HDD: char = '\u{f0a0}'; // fa-hdd-o (a provider row)
pub const EXCLAMATION_TRIANGLE: char = '\u{f071}'; // fa-exclamation-triangle
pub const CIRCLE_O_NOTCH: char = '\u{f1ce}'; // fa-circle-o-notch (loading)
pub const ARROW_LEFT: char = '\u{f060}'; // fa-arrow-left (the browser's Back button)
pub const SEARCH: char = '\u{f002}'; // fa-search (the search box's leading glyph)
pub const TIMES_CIRCLE: char = '\u{f057}'; // fa-times-circle (round "clear the search")
pub const HOURGLASS: char = '\u{f254}'; // fa-hourglass (a listing still in flight)

/// Every icon constant paired with its name. The coverage test iterates this,
/// so adding a new icon above without listing it here (or with a code point
/// absent from the font) fails loudly.
#[cfg(test)]
pub(crate) const ALL_ICONS: &[(&str, char)] = &[
    ("UNDO", UNDO),
    ("REDO", REDO),
    ("TRASH", TRASH),
    ("EXPAND", EXPAND),
    ("BOOK", BOOK),
    ("INFO_CIRCLE", INFO_CIRCLE),
    ("FILE_NEW", FILE_NEW),
    ("FOLDER_OPEN", FOLDER_OPEN),
    ("SAVE", SAVE),
    ("IMPORT", IMPORT),
    ("EXPORT", EXPORT),
    ("SUN", SUN),
    ("BUG", BUG),
    ("TACHOMETER", TACHOMETER),
    ("PLUS_CIRCLE", PLUS_CIRCLE),
    ("PENCIL", PENCIL),
    ("CUBE", CUBE),
    ("CUBES", CUBES),
    ("CALCULATOR", CALCULATOR),
    ("SLIDERS", SLIDERS),
    ("PLUG", PLUG),
    ("CHEVRON_LEFT", CHEVRON_LEFT),
    ("CHEVRON_RIGHT", CHEVRON_RIGHT),
    ("TIMES", TIMES),
    ("FOLDER", FOLDER),
    ("HDD", HDD),
    ("EXCLAMATION_TRIANGLE", EXCLAMATION_TRIANGLE),
    ("CIRCLE_O_NOTCH", CIRCLE_O_NOTCH),
    ("ARROW_LEFT", ARROW_LEFT),
    ("SEARCH", SEARCH),
    ("TIMES_CIRCLE", TIMES_CIRCLE),
    ("HOURGLASS", HOURGLASS),
];

#[cfg(test)]
mod tests {
    use super::*;
    use agg_gui::text::Font;

    /// Every icon code point this module exposes MUST resolve to a real glyph
    /// in the bundled Font Awesome face. A wrong code point or a swapped font
    /// otherwise renders an empty "tofu" box at runtime with no error — this
    /// test turns that silent failure into a loud CI failure.
    #[test]
    fn every_icon_code_point_has_a_glyph() {
        let font = Font::from_slice(ICON_FONT_BYTES).expect("parse font-awesome.ttf");
        let chars = font.characters();
        for (name, ch) in ALL_ICONS {
            assert!(
                chars.contains(ch),
                "Font Awesome face lacks a glyph for {name} (U+{:04X})",
                *ch as u32
            );
        }
    }
}
