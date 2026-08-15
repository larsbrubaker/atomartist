//! Inside one browser card — the geometry and the small text/colour
//! decisions [`super::widget_paint`] needs to draw it (step 6f-3).
//!
//! Split from `widget_geom.rs` (which owns the *outer* regions and the
//! tiling) and from `widget_paint.rs` (which owns the drawing) so the
//! card's internals stay pure functions: no `DrawCtx`, no `AppState`, no
//! frame. NodeDesigner's card — `file-browser-file-grid.js` — is a
//! centred column of an 80×60 `object-fit: cover` thumbnail, the entry
//! name *below* it at 12 px with word-break, and an optional 10 px grey
//! modified date, and that is exactly what this file lays out.
//!
//! Coordinates are widget-local and **Y-up**, like everything else in the
//! browser: a card's contents stack *downward* from `card.y +
//! card.height`.

use agg_gui::{Color, Rect};
use atomartist_storage::{Entry, ModifiedMs};

use super::widget_geom::{
    self as geom, CARD_PAD_X, CARD_PAD_Y, DATE_LINE_H, NAME_LINES, NAME_LINE_H, NAME_SIZE, THUMB_H,
    THUMB_NAME_GAP, THUMB_W,
};
use crate::app_state_storage::uri_extension;

/// Where each piece of a card lands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CardLayout {
    /// The 80×60 thumbnail frame, centred across the card's top.
    pub thumb: Rect,
    /// Line boxes for the name, top line first. Always
    /// [`NAME_LINES`] long; the caller draws only the lines it filled.
    pub name_lines: [Rect; NAME_LINES],
    /// The modified-date line at the card's bottom.
    pub date: Rect,
}

/// Carve `card` (a rectangle from [`geom::cell_rect`]) into its parts.
pub fn card_layout(card: Rect) -> CardLayout {
    let inner_x = card.x + CARD_PAD_X;
    let inner_w = (card.width - CARD_PAD_X * 2.0).max(0.0);
    let thumb_w = THUMB_W.min(inner_w.max(0.0));
    let thumb = Rect::new(
        card.x + (card.width - thumb_w) * 0.5,
        card.y + card.height - CARD_PAD_Y - THUMB_H,
        thumb_w,
        THUMB_H,
    );
    let first_top = thumb.y - THUMB_NAME_GAP;
    let name_lines = std::array::from_fn(|i| {
        Rect::new(
            inner_x,
            first_top - (i + 1) as f64 * NAME_LINE_H,
            inner_w,
            NAME_LINE_H,
        )
    });
    let date = Rect::new(inner_x, card.y + CARD_PAD_Y, inner_w, DATE_LINE_H);
    CardLayout {
        thumb,
        name_lines,
        date,
    }
}

/// Break `name` into at most [`NAME_LINES`] lines that each fit `max_w`.
///
/// Word-wrap first (ND's `word-break` on a flex column), falling back to
/// breaking inside an over-long word — a file name is often one
/// unbroken token, so a pure word wrapper would overflow constantly. The
/// last line is elided when there is more text than fits.
pub fn wrap_name(name: &str, max_w: f64) -> Vec<String> {
    if max_w <= 0.0 || name.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut rest = name.split_whitespace().peekable();
    while let Some(word) = rest.next() {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if geom::measure(&candidate, NAME_SIZE) <= max_w {
            current = candidate;
            continue;
        }
        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            if lines.len() == NAME_LINES {
                return finish(lines, Some(word), rest.peek().is_some(), max_w);
            }
        }
        // The word alone still does not fit: break it by characters,
        // as many times as it takes (a file name is often one long
        // unbroken token).
        let mut remaining = word.to_string();
        loop {
            let (head, tail) = split_to_width(&remaining, max_w);
            if tail.is_empty() {
                current = head;
                break;
            }
            lines.push(head);
            if lines.len() == NAME_LINES {
                return finish(lines, Some(&tail), rest.peek().is_some(), max_w);
            }
            remaining = tail;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines.truncate(NAME_LINES);
    lines
}

/// Close out a wrap that ran out of lines: the last line gets an ellipsis
/// because `leftover` (and possibly more words) never made it on.
fn finish(mut lines: Vec<String>, leftover: Option<&str>, more: bool, max_w: f64) -> Vec<String> {
    if leftover.is_some() || more {
        if let Some(last) = lines.last_mut() {
            *last = ellipsize(last, max_w);
        }
    }
    lines
}

/// Append an ellipsis to `text`, dropping trailing characters until the
/// result fits `max_w`. Unlike [`elide`] this *always* adds one — the
/// caller has already established that there is more text than lines.
fn ellipsize(text: &str, max_w: f64) -> String {
    let mut kept: Vec<char> = text.chars().collect();
    loop {
        let candidate: String = kept.iter().collect::<String>() + "…";
        if kept.is_empty() || geom::measure(&candidate, NAME_SIZE) <= max_w {
            return candidate;
        }
        kept.pop();
    }
}

/// Longest prefix of `word` that fits `max_w`, plus the remainder. At
/// least one character always moves, so a caller looping on the tail
/// cannot spin forever.
fn split_to_width(word: &str, max_w: f64) -> (String, String) {
    let mut head = String::new();
    for (i, ch) in word.char_indices() {
        let mut candidate = head.clone();
        candidate.push(ch);
        if !head.is_empty() && geom::measure(&candidate, NAME_SIZE) > max_w {
            return (head, word[i..].to_string());
        }
        head = candidate;
    }
    (head, String::new())
}

/// What an entry *is*, as far as the card's accent colour is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryAccent {
    Folder,
    Mesh,
    File,
}

impl EntryAccent {
    /// NodeDesigner's palette: folders warm yellow, meshes violet,
    /// everything else the neutral file blue.
    pub fn color(self) -> Color {
        match self {
            // #f0c674
            EntryAccent::Folder => Color::rgb(0.941, 0.776, 0.455),
            // #b07cb0
            EntryAccent::Mesh => Color::rgb(0.690, 0.486, 0.690),
            // #6ca4dc
            EntryAccent::File => Color::rgb(0.424, 0.643, 0.863),
        }
    }
}

/// Classify an entry. Meshes are the import formats the scene can take —
/// the same list the drag-insert payload uses, so "looks like a mesh" and
/// "can be dropped into the scene" cannot drift apart.
pub fn accent_of(entry: &Entry) -> EntryAccent {
    if entry.is_dir {
        return EntryAccent::Folder;
    }
    let ext = uri_extension(&entry.uri);
    if crate::app_state_files_import::MESH_IMPORT_EXTENSIONS.contains(&ext.as_str()) {
        EntryAccent::Mesh
    } else {
        EntryAccent::File
    }
}

/// Destination rectangle that makes an `iw × ih` image *cover* `frame`:
/// scaled up until both axes are filled and centred, so the overflow is
/// cropped evenly. The caller clips to `frame` — this only decides where
/// the image goes.
pub fn cover_rect(frame: Rect, iw: u32, ih: u32) -> Rect {
    let iw = iw.max(1) as f64;
    let ih = ih.max(1) as f64;
    let scale = (frame.width / iw).max(frame.height / ih);
    let w = iw * scale;
    let h = ih * scale;
    Rect::new(
        frame.x + (frame.width - w) * 0.5,
        frame.y + (frame.height - h) * 0.5,
        w,
        h,
    )
}

/// `YYYY-MM-DD` for a Unix-epoch millisecond stamp.
///
/// Hand-rolled rather than pulled from a date crate: the browser needs
/// one civil date and nothing else (no time zones, no parsing, no
/// formatting language), and the workspace has no date dependency to
/// inherit. This is Howard Hinnant's `civil_from_days`, the standard
/// proleptic-Gregorian algorithm, evaluated in UTC.
pub fn format_modified(modified: ModifiedMs) -> String {
    let days = (modified / 86_400_000) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
#[path = "widget_cards_tests.rs"]
mod widget_cards_tests;
