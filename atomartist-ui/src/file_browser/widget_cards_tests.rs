//! Unit tests for [`super`] — one browser card's internals.
//!
//! Pure arithmetic and pure strings, so no `AppState`, no font, and no
//! frame is needed. The headless `geom::measure` fallback (a
//! proportional estimate) is what makes the wrapping assertions stable
//! here.

use super::*;
use agg_gui::Rect;
use atomartist_storage::{Entry, Stamp, StorageUri};

fn card() -> Rect {
    Rect::new(20.0, 40.0, 140.0, geom::CARD_H)
}

fn file(name: &str) -> Entry {
    Entry::file(
        StorageUri::new("mem", &format!("/{name}")),
        1,
        Stamp::new("s"),
    )
}

/// The card is a centred column: thumbnail on top, name below it, date
/// at the bottom — ND's order, and the one that made "name below the
/// thumbnail" a 6f-3 requirement.
#[test]
fn card_stacks_thumbnail_then_name_then_date_downward() {
    let card = card();
    let layout = card_layout(card);

    assert_eq!(layout.thumb.width, THUMB_W);
    assert_eq!(layout.thumb.height, THUMB_H);
    // Y-up: the thumbnail's top edge is the card's top, minus padding.
    assert!(
        (layout.thumb.y + layout.thumb.height - (card.y + card.height - CARD_PAD_Y)).abs() < 1e-9
    );
    // Centred across the card.
    let left = layout.thumb.x - card.x;
    let right = card.x + card.width - (layout.thumb.x + layout.thumb.width);
    assert!((left - right).abs() < 1e-9, "thumbnail is centred");

    // Name lines sit *below* the thumbnail and run downward.
    assert!(layout.name_lines[0].y + layout.name_lines[0].height <= layout.thumb.y);
    assert!(layout.name_lines[1].y < layout.name_lines[0].y);
    // Date is below both, inside the card.
    assert!(layout.date.y + layout.date.height <= layout.name_lines[1].y);
    assert!(layout.date.y >= card.y);
}

/// The name wraps to at most two lines and elides what is left over.
#[test]
fn wrap_name_fills_two_lines_then_elides() {
    let wide = 400.0;
    assert_eq!(wrap_name("short.atmr", wide), vec!["short.atmr"]);

    // Narrow enough that a two-word name needs both lines.
    let narrow = geom::measure("robot", NAME_SIZE) + 1.0;
    let lines = wrap_name("robot arm assembly rev 3", narrow);
    assert_eq!(lines.len(), NAME_LINES, "never more than two lines");
    assert!(
        lines[1].ends_with('…'),
        "the leftover must be signalled, got {lines:?}"
    );

    // One unbroken token longer than two lines is broken by characters.
    let lines = wrap_name("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.atmr", narrow);
    assert_eq!(lines.len(), NAME_LINES);
    assert!(lines[0].len() > 1, "the token is split, not dropped");
    assert!(lines[1].ends_with('…'));

    assert!(wrap_name("anything", 0.0).is_empty(), "no width, no lines");
}

/// A multibyte name must wrap, break and elide on *character*
/// boundaries — every one of these helpers indexes a `&str`, so a byte
/// slice in the middle of a CJK ideograph or an emoji would panic on a
/// perfectly ordinary file name.
#[test]
fn wrap_name_breaks_multibyte_names_on_character_boundaries() {
    // 3-byte CJK, no whitespace to wrap at: the char-breaking path.
    let cjk = "設計図面の最終版データ一式.atmr";
    for width in [8.0, 20.0, 45.0, 120.0, 400.0] {
        let lines = wrap_name(cjk, width);
        assert!(lines.len() <= NAME_LINES, "{width}: {lines:?}");
        // Nothing is corrupted: every line is a contiguous run of the
        // original's characters (ellipsis aside).
        for line in &lines {
            for ch in line.chars().filter(|c| *c != '…') {
                assert!(cjk.contains(ch), "{width}: {line:?} invented {ch:?}");
            }
        }
    }

    // 4-byte emoji, including whitespace so both the word path and the
    // char path run.
    let emoji = "🚀 rocket 🛠️ parts 🧩.stl";
    for width in [6.0, 30.0, 90.0, 500.0] {
        let lines = wrap_name(emoji, width);
        assert!(lines.len() <= NAME_LINES, "{width}: {lines:?}");
    }

    // The two building blocks directly, at a width narrower than one
    // character — the boundary case that makes `split_to_width` always
    // move at least one char rather than spin.
    let (head, tail) = split_to_width("漢字テスト", 1.0);
    assert_eq!(head.chars().count(), 1, "at least one character moves");
    assert_eq!(format!("{head}{tail}"), "漢字テスト");
    assert!(ellipsize("漢字テスト", 1.0).ends_with('…'));
}

/// Type accents follow ND's palette, and a mesh is exactly what the
/// scene can import.
#[test]
fn accents_distinguish_folder_mesh_and_file() {
    let dir = Entry::dir(StorageUri::new("mem", "/docs"));
    assert_eq!(accent_of(&dir), EntryAccent::Folder);
    assert_eq!(accent_of(&file("part.stl")), EntryAccent::Mesh);
    assert_eq!(accent_of(&file("part.3mf")), EntryAccent::Mesh);
    assert_eq!(accent_of(&file("design.atmr")), EntryAccent::File);
    assert_eq!(accent_of(&file("notes.txt")), EntryAccent::File);

    // #f0c674 / #b07cb0 / #6ca4dc, to a byte.
    let folder = EntryAccent::Folder.color();
    assert_eq!((folder.r * 255.0).round() as u8, 0xf0);
    assert_eq!((folder.g * 255.0).round() as u8, 0xc6);
    assert_eq!((folder.b * 255.0).round() as u8, 0x74);
    let mesh = EntryAccent::Mesh.color();
    assert_eq!((mesh.r * 255.0).round() as u8, 0xb0);
    assert_eq!((mesh.b * 255.0).round() as u8, 0xb0);
    let plain = EntryAccent::File.color();
    assert_eq!((plain.r * 255.0).round() as u8, 0x6c);
    assert_eq!((plain.b * 255.0).round() as u8, 0xdc);
}

/// `cover` fills the frame on both axes and centres the overflow —
/// unlike `contain`, it never leaves a letterbox.
#[test]
fn cover_rect_fills_the_frame_and_centres_the_crop() {
    let frame = Rect::new(10.0, 10.0, 80.0, 60.0);

    // Wide image: height is the binding axis, width overflows evenly.
    let wide = cover_rect(frame, 200, 100);
    assert!(wide.width >= frame.width - 1e-9);
    assert!((wide.height - frame.height).abs() < 1e-9);
    let left_crop = frame.x - wide.x;
    let right_crop = (wide.x + wide.width) - (frame.x + frame.width);
    assert!(left_crop > 0.0, "a wide image really does overflow");
    assert!(
        (left_crop - right_crop).abs() < 1e-9,
        "the crop is even on both sides"
    );

    // Tall image: width binds instead.
    let tall = cover_rect(frame, 100, 400);
    assert!((tall.width - frame.width).abs() < 1e-9);
    assert!(tall.height >= frame.height - 1e-9);

    // A degenerate image must not produce NaN.
    let none = cover_rect(frame, 0, 0);
    assert!(none.width.is_finite() && none.height.is_finite());
}

/// The date line is a plain UTC civil date.
#[test]
fn format_modified_renders_a_civil_date() {
    assert_eq!(format_modified(0), "1970-01-01");
    // 2001-09-09T01:46:40Z, the "billion seconds" mark.
    assert_eq!(format_modified(1_000_000_000_000), "2001-09-09");
    // A leap day.
    assert_eq!(format_modified(1_709_164_800_000), "2024-02-29");
}
