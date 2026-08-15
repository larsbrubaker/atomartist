//! Geometry for the left favorites bar — where the handle, the favorite
//! rows, and the embedded browser land inside
//! [`FavoritesBar`](crate::favorites_bar::FavoritesBar)'s bounds.
//!
//! Split out of `favorites_bar.rs` so that file stays assembly + event
//! routing and this one stays pure arithmetic: every function here is a
//! free function over rectangles, testable without an
//! [`AppState`](crate::AppState), a font, or a frame. `favorites_bar_paint`
//! consumes exactly the rectangles the widget hit-tests, so what the user
//! clicks is by construction what the user sees.
//!
//! # Coordinate system
//!
//! Widget-local and **Y-up** (agg-gui's convention, CLAUDE.md): the origin
//! is the bar's bottom-left corner and `y + height` is its top edge. The
//! favorites therefore stack *downward* from the top — row 0 has the
//! highest Y — which is the "top-down glyph stacking" the parts-bar
//! ancestor draws, expressed in bottom-up coordinates.
//!
//! The bar is docked on the canvas's **left** edge, so the resize handle
//! sits on its *right* side and dragging right widens it.

use agg_gui::{Rect, Size};

/// Width of the collapsed icon rail, handle included.
pub const RAIL_W: f64 = 38.0;
/// Width of the grab strip on the bar's right edge — the toggle button
/// and the resize grip, as in `parts-bar.js`.
pub const HANDLE_W: f64 = 6.0;
/// Padding around the bar's contents.
pub const PAD: f64 = 4.0;
/// One favorite in the collapsed rail (glyph only).
pub const RAIL_ROW_H: f64 = 32.0;
/// One favorite in the expanded panel (glyph + label + unpin).
pub const ROW_H: f64 = 24.0;
/// Width of the per-row unpin (×) hit target in the expanded panel.
pub const UNPIN_W: f64 = 18.0;
/// Largest share of the bar's height the favorites list may take before
/// the embedded browser starts losing room.
pub const MAX_LIST_FRACTION: f64 = 0.45;

/// Where each piece of the bar lands, bar-local and Y-up.
#[derive(Debug, Clone, PartialEq)]
pub struct BarLayout {
    /// Grab strip on the right edge: toggle *and* resize grip.
    pub handle: Rect,
    /// Everything left of the handle.
    pub content: Rect,
    /// The favorites, top-down. Only the rows that fit are produced, so
    /// paint and hit-test can never disagree about a clipped row.
    pub rows: Vec<Rect>,
    /// "Pin current project" affordance, under the last row. `None` when
    /// the caller said there is nothing to pin, or nothing fits.
    pub pin: Option<Rect>,
    /// The embedded [`FileBrowser`](crate::file_browser::FileBrowser),
    /// filling what is left under the list. `None` while collapsed.
    pub browser: Option<Rect>,
}

/// Carve a bar of `available` size.
///
/// `count` is how many favorites want a row and `show_pin` whether the
/// pin-current-project affordance is wanted; both are requests, and the
/// returned rectangles are what actually fits. Degenerate sizes yield
/// zero-area rectangles rather than negative ones.
pub fn compute(available: Size, expanded: bool, count: usize, show_pin: bool) -> BarLayout {
    let w = available.width.max(0.0);
    let h = available.height.max(0.0);
    let handle_w = HANDLE_W.min(w);
    let handle = Rect::new(w - handle_w, 0.0, handle_w, h);
    let content = Rect::new(0.0, 0.0, (w - handle_w).max(0.0), h);

    let row_h = if expanded { ROW_H } else { RAIL_ROW_H };
    // Collapsed, the rail is nothing *but* favorites, so the list may use
    // the whole height; expanded, it yields to the browser.
    let list_budget = if expanded { h * MAX_LIST_FRACTION } else { h };
    let fits = if row_h > 0.0 {
        (((list_budget - PAD).max(0.0)) / row_h).floor() as usize
    } else {
        0
    };
    // The pin affordance gets its slot *first*: a list longer than the
    // budget is the case where pinning is most likely to be wanted, and
    // an action that vanishes once you have seven favourites is worse
    // than one favourite going unlisted. (v1 has no scrolling list —
    // favourites past `fits` simply are not placed. 6e's reorder work is
    // the natural place to add one.)
    let row_slots = fits.saturating_sub(usize::from(show_pin));
    let placed = count.min(row_slots);

    let mut rows = Vec::with_capacity(placed);
    let mut top = content.y + content.height - PAD;
    for _ in 0..placed {
        rows.push(Rect::new(content.x, top - row_h, content.width, row_h));
        top -= row_h;
    }
    let pin = (show_pin && fits > 0).then(|| {
        let rect = Rect::new(content.x, top - row_h, content.width, row_h);
        top -= row_h;
        rect
    });

    let browser = expanded.then(|| {
        let bottom = content.y;
        let list_bottom = (top - PAD).max(bottom);
        Rect::new(
            content.x,
            bottom,
            content.width,
            (list_bottom - bottom).max(0.0),
        )
    });

    BarLayout {
        handle,
        content,
        rows,
        pin,
        browser,
    }
}

/// Unpin (×) target inside an expanded row — the right end of it.
pub fn unpin_rect(row: Rect) -> Rect {
    let w = UNPIN_W.min(row.width);
    Rect::new(row.x + row.width - w - PAD, row.y, w, row.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(w: f64, h: f64) -> Size {
        Size::new(w, h)
    }

    /// The handle hugs the bar's right edge (the canvas side) so a drag
    /// to the right widens the bar — the one place the "docked left"
    /// decision shows up in arithmetic.
    #[test]
    fn handle_sits_on_the_right_edge() {
        let layout = compute(size(RAIL_W, 400.0), false, 3, false);
        assert_eq!(layout.handle.x + layout.handle.width, RAIL_W);
        assert_eq!(layout.content.width, RAIL_W - HANDLE_W);
    }

    /// Y-up: row 0 is the *topmost*, i.e. the one with the highest Y.
    #[test]
    fn rail_rows_stack_downward_from_the_top() {
        let layout = compute(size(RAIL_W, 400.0), false, 3, false);
        assert_eq!(layout.rows.len(), 3);
        assert!(layout.rows[0].y > layout.rows[1].y);
        assert!(layout.rows[1].y > layout.rows[2].y);
        assert_eq!(layout.rows[0].y + layout.rows[0].height, 400.0 - PAD);
        assert!(
            layout.browser.is_none(),
            "a collapsed rail hosts no browser"
        );
    }

    /// Rows that would not fit are not produced at all, so a click can
    /// never land on a row the user cannot see.
    #[test]
    fn rows_that_do_not_fit_are_dropped() {
        let layout = compute(size(RAIL_W, 3.0 * RAIL_ROW_H + PAD), false, 10, false);
        assert_eq!(layout.rows.len(), 3);
    }

    /// Expanded, the list is capped and the browser takes the rest.
    #[test]
    fn expanded_panel_splits_list_and_browser() {
        let layout = compute(size(220.0, 400.0), true, 7, false);
        let browser = layout.browser.expect("an expanded panel hosts the browser");
        assert!(browser.height > 400.0 * (1.0 - MAX_LIST_FRACTION) - PAD * 2.0);
        assert!(
            browser.y + browser.height <= layout.rows.last().unwrap().y,
            "the browser must not overlap the last row"
        );
    }

    /// The pin affordance keeps its slot even when that costs a
    /// favourite its row — the action must not disappear on a long list.
    #[test]
    fn pin_row_is_reserved_before_the_favorites() {
        let tight = compute(size(RAIL_W, 3.0 * RAIL_ROW_H + PAD), false, 3, true);
        assert_eq!(tight.rows.len(), 2);
        let pin = tight.pin.expect("the pin row keeps its slot");
        assert!(
            pin.y < tight.rows.last().unwrap().y,
            "and sits under the last favourite"
        );
        let roomy = compute(size(RAIL_W, 4.0 * RAIL_ROW_H + PAD), false, 3, true);
        assert_eq!(roomy.rows.len(), 3);
        assert!(roomy.pin.is_some());
    }

    /// The configuration production actually produces: expanded, the
    /// seeded palette (7), a pinnable project, and a canvas-pane height
    /// where `MAX_LIST_FRACTION` bites. The pin row must survive that —
    /// this is the case the collapsed test above cannot reach, and the
    /// one that made the reserve-first rule necessary.
    #[test]
    fn expanded_pin_row_survives_the_list_budget() {
        // 260 px is about what the node-canvas pane gets in a 1280×720
        // window; the list budget is then 117 px = 4 rows of 24.
        let h = 260.0;
        let layout = compute(size(300.0, h), true, 7, true);
        let fits = ((h * MAX_LIST_FRACTION - PAD) / ROW_H).floor() as usize;
        assert!(fits < 7, "the budget must actually bite in this fixture");
        assert_eq!(
            layout.rows.len(),
            fits - 1,
            "the pin row costs the list one slot"
        );
        let pin = layout.pin.expect("an expanded panel keeps the pin row");
        assert!(pin.y < layout.rows.last().unwrap().y);
        let browser = layout.browser.expect("and still hosts the browser");
        assert!(
            browser.y + browser.height <= pin.y,
            "the browser must not overlap the pin row"
        );
    }

    /// Degenerate sizes stay non-negative — the widget paints and
    /// hit-tests them without special cases.
    #[test]
    fn zero_size_bar_has_no_negative_rects() {
        let layout = compute(size(0.0, 0.0), true, 5, true);
        assert!(layout.content.width >= 0.0 && layout.content.height >= 0.0);
        assert!(layout.rows.is_empty());
        assert_eq!(layout.browser.map(|r| r.height), Some(0.0));
    }
}
