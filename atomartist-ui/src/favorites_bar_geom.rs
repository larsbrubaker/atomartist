//! Geometry for the left favorites bar — where the persistent icon
//! strip, the expanded browser panel, and the resize handle land inside
//! [`FavoritesBar`](crate::favorites_bar::FavoritesBar)'s bounds
//! (`docs/file-browser-design.md` §5b, step 6f-1).
//!
//! Split out of `favorites_bar.rs` so that file stays assembly + event
//! routing and this one stays pure arithmetic: every function here is a
//! free function over rectangles, testable without an
//! [`AppState`](crate::AppState), a font, or a frame. `favorites_strip`
//! and `favorites_bar_paint` consume exactly the rectangles the widget
//! hit-tests, so what the user clicks is by construction what the user
//! sees.
//!
//! # Structure (6g-2)
//!
//! NodeDesigner's parts bar (`static/js/node-editor/ui/parts-bar.js`) is
//! docked on the window's **right** edge and orders its DOM
//! viewport → handle + strip → panel: the strip is always the piece
//! *nearest the viewport* and the panel grows away from it, outward. We
//! dock on the **left**, so the mirror image is
//!
//! ```text
//!   | panel (absent when collapsed) | strip (72) |
//!                                        ^ grip floats on this edge
//! ```
//!
//! The strip **never collapses** — it is the primitive palette and is
//! always on screen — and it always sits at the bar's *viewport-facing*
//! edge. Expanding inserts the browser panel between the window edge and
//! the strip; the bar's total width grows, the viewport shrinks, and the
//! strip stays exactly where the user's hand expects it: right beside the
//! viewport. That is why the persisted width is the *panel's*, not the
//! bar's.
//!
//! # The handle is only the grip (6g-2)
//!
//! There is no full-height reserved lane for the handle. The strip's
//! right edge **is** the bar's right edge, and the 16 × 56 grip is drawn
//! (and hit-tested) on top of it, vertically centered — ND's
//! `align-self: center` affordance with `border-radius: 6px 0 0 6px`.
//! Everything above and below the grip belongs to whatever is underneath
//! it: the strip's items, or the strip's background. The 16 px that used
//! to be reserved goes to the 3-D viewport.
//!
//! # Coordinate system
//!
//! Widget-local and **Y-up** (agg-gui's convention, CLAUDE.md): the origin
//! is the bar's bottom-left corner and `y + height` is its top edge. The
//! favorites therefore stack *downward* from the top — item 0 has the
//! highest Y — which is the "top-down icon stacking" the parts-bar
//! ancestor draws, expressed in bottom-up coordinates.
//!
//! Because the bar is docked on the pane's **left** edge, the resize
//! handle sits on its *right* side and dragging right widens the panel.

use agg_gui::{Rect, Size};

/// Width of the persistent icon strip. NodeDesigner's collapsed rail
/// width; ours never shrinks below it.
pub const STRIP_W: f64 = 72.0;
/// Width of the grip on the bar's right edge — the toggle button *and*
/// the resize handle, as in `parts-bar.js` (16 × 56). It floats **over**
/// the strip's right edge rather than reserving a column of its own.
pub const HANDLE_W: f64 = 16.0;
/// Height of the grip, vertically centered in the bar.
pub const HANDLE_H: f64 = 56.0;
/// Width the bar occupies with the panel closed. Since 6g-2 that is the
/// strip alone: the grip is drawn on top of it, not beside it.
pub const COLLAPSED_W: f64 = STRIP_W;
/// Padding around the bar's contents.
pub const PAD: f64 = 4.0;
/// The square icon slot inside one strip item (ND's 44 × 44 tile).
pub const ICON_SLOT: f64 = 44.0;
/// Label font size under a strip icon (ND's 9 px).
pub const LABEL_SIZE: f64 = 9.0;
/// Line box the label occupies under the slot.
pub const LABEL_LINE_H: f64 = 12.0;
/// Vertical padding inside one strip item.
pub const ITEM_PAD: f64 = 3.0;
/// One strip item: icon slot plus its label line.
pub const ITEM_H: f64 = ITEM_PAD * 2.0 + ICON_SLOT + LABEL_LINE_H;

/// Where each piece of the bar lands, bar-local and Y-up.
#[derive(Debug, Clone, PartialEq)]
pub struct BarLayout {
    /// The persistent icon strip, anchored to the bar's **right** (i.e.
    /// viewport-facing) edge. Always present, collapsed or not.
    pub strip: Rect,
    /// The favourites, top-down inside the strip — **one rectangle per
    /// favourite**, scrolled by the caller's offset. Entries outside
    /// [`items_viewport`](Self::items_viewport) are off-screen; paint
    /// clips to that viewport and hit-testing requires it, so the two
    /// can never disagree about a scrolled-away item.
    pub items: Vec<Rect>,
    /// The scrollable region the items live in: the strip minus its
    /// padding and minus the pin's anchored slot.
    pub items_viewport: Rect,
    /// "Pin current project" affordance, anchored to the strip's
    /// **bottom** and deliberately *outside* the scroll region — the
    /// ancestor keeps its actions reachable. `None` when the caller said
    /// there is nothing to pin, or nothing fits.
    pub pin: Option<Rect>,
    /// The embedded [`FileBrowser`](crate::file_browser::FileBrowser),
    /// between the window edge and the strip. `None` while collapsed.
    pub panel: Option<Rect>,
    /// The 16 × 56 grip on the bar's right edge: toggle *and* resize.
    /// Overlaps the strip — it is drawn on top of it — so hit-tests must
    /// consult it **before** the strip's items, which is what
    /// [`FavoritesBar`](crate::favorites_bar::FavoritesBar) does.
    pub handle: Rect,
}

/// Carve a bar of `available` size.
///
/// `panel_open` is whether the browser panel is showing (the strip shows
/// either way); `count` is how many favorites want an item, `show_pin`
/// whether the pin-current-project affordance is wanted, and `scroll` how
/// far the item list has been scrolled (0 = top, growing downward — see
/// [`max_scroll`]). Every favourite gets a rectangle; the ones outside
/// `items_viewport` are simply scrolled off. Degenerate sizes yield
/// zero-area rectangles rather than negative ones.
pub fn compute(
    available: Size,
    panel_open: bool,
    count: usize,
    show_pin: bool,
    scroll: f64,
) -> BarLayout {
    let w = available.width.max(0.0);
    let h = available.height.max(0.0);
    // The strip hugs the bar's right edge — the side the 3-D viewport is
    // on — whatever else the bar is showing, so opening the panel never
    // moves it away from the viewport.
    let strip_w = STRIP_W.min(w);
    let panel_w = (w - strip_w).max(0.0);
    let strip = Rect::new(panel_w, 0.0, strip_w, h);
    let panel = panel_open.then(|| Rect::new(0.0, 0.0, panel_w, h));
    // The grip is a floating affordance drawn over the strip's right
    // edge, not a lane carved out of the bar.
    let handle_w = HANDLE_W.min(w);
    let handle_h = HANDLE_H.min(h);
    let handle = Rect::new(w - handle_w, (h - handle_h) * 0.5, handle_w, handle_h);

    // The pin affordance gets its slot *first*, anchored to the bottom
    // and outside the scroll region: an action that scrolls away is an
    // action the user cannot find (the ancestor pins its own actions the
    // same way).
    let usable = (h - PAD * 2.0).max(0.0);
    let pin = (show_pin && usable >= ITEM_H)
        .then(|| Rect::new(strip.x, strip.y + PAD, strip.width, ITEM_H));
    let viewport_bottom = match pin {
        Some(pin) => pin.y + pin.height,
        None => strip.y + PAD,
    };
    let items_viewport = Rect::new(
        strip.x,
        viewport_bottom,
        strip.width,
        (strip.y + strip.height - PAD - viewport_bottom).max(0.0),
    );

    // Y-up: item 0 is at the top of the viewport, and scrolling moves the
    // whole column *up* (positive `scroll`) to reveal later favourites.
    let top = items_viewport.y + items_viewport.height + scroll;
    let items = (0..count)
        .map(|i| {
            Rect::new(
                strip.x,
                top - ITEM_H * (i as f64 + 1.0),
                strip.width,
                ITEM_H,
            )
        })
        .collect();

    BarLayout {
        strip,
        items,
        items_viewport,
        pin,
        panel,
        handle,
    }
}

/// Furthest the item column may be scrolled: everything below the
/// viewport, and never less than zero (a palette that fits does not
/// scroll at all).
pub fn max_scroll(available: Size, count: usize, show_pin: bool) -> f64 {
    let layout = compute(available, false, count, show_pin, 0.0);
    (count as f64 * ITEM_H - layout.items_viewport.height).max(0.0)
}

/// Is `item` at least partly on screen inside `viewport`?
pub fn item_visible(item: Rect, viewport: Rect) -> bool {
    item.y + item.height > viewport.y && item.y < viewport.y + viewport.height
}

/// The 44 × 44 icon slot at the top of one strip item.
pub fn icon_slot(item: Rect) -> Rect {
    let w = ICON_SLOT.min(item.width);
    let h = ICON_SLOT.min(item.height);
    Rect::new(
        item.x + (item.width - w) * 0.5,
        (item.y + item.height - ITEM_PAD - h).max(item.y),
        w,
        h,
    )
}

/// The single-line label box under the icon slot (Y-up: the item's
/// bottom).
pub fn label_box(item: Rect) -> Rect {
    let h = LABEL_LINE_H.min(item.height);
    Rect::new(item.x, item.y + ITEM_PAD, item.width, h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agg_gui::Point;

    fn size(w: f64, h: f64) -> Size {
        Size::new(w, h)
    }

    /// Collapsed = the strip alone (6g-2: the handle no longer reserves a
    /// lane), and the strip is *still there* — the 6f-1 rule the 6d-2
    /// rail broke.
    #[test]
    fn collapsed_bar_is_the_strip_alone() {
        let layout = compute(size(COLLAPSED_W, 400.0), false, 3, false, 0.0);
        assert_eq!(COLLAPSED_W, STRIP_W, "no reserved handle lane");
        assert_eq!(layout.strip.width, STRIP_W);
        assert_eq!(layout.strip.x, 0.0);
        assert_eq!(layout.items.len(), 3, "the strip never collapses");
        assert!(layout.panel.is_none(), "no panel while collapsed");
        // The grip floats *over* the strip's right edge.
        assert_eq!(layout.handle.x + layout.handle.width, COLLAPSED_W);
        assert_eq!(layout.handle.width, HANDLE_W);
        assert_eq!(layout.handle.height, HANDLE_H);
        assert!(
            layout.handle.x >= layout.strip.x
                && layout.handle.x + layout.handle.width <= layout.strip.x + layout.strip.width,
            "the grip is inside the strip, not beside it"
        );
    }

    /// Expanded, the panel goes *outboard* of the strip — 6g-2's order —
    /// and the strip keeps both its full width and its place against the
    /// bar's viewport-facing edge.
    #[test]
    fn expanded_bar_puts_the_panel_outboard_of_the_strip() {
        let total = COLLAPSED_W + 380.0;
        let layout = compute(size(total, 7.0 * ITEM_H + PAD * 2.0), true, 7, false, 0.0);
        let panel = layout.panel.expect("an expanded bar hosts the panel");
        assert_eq!(layout.strip.width, STRIP_W);
        assert_eq!(panel.x, 0.0, "the panel starts at the window edge");
        assert_eq!(panel.width, 380.0);
        assert_eq!(panel.x + panel.width, layout.strip.x, "then the strip");
        assert_eq!(
            layout.strip.x + layout.strip.width,
            total,
            "and the strip still ends at the bar's viewport edge"
        );
        assert_eq!(
            layout.items.len(),
            7,
            "the strip shows the same palette either way"
        );
    }

    /// The invariant behind 6g-2: whatever the bar is showing, the strip
    /// ends exactly at the bar's right (viewport-facing) edge, so
    /// collapsing moves only the panel.
    #[test]
    fn the_strip_stays_at_the_viewport_edge_in_both_states() {
        for (open, w) in [(false, COLLAPSED_W), (true, COLLAPSED_W + 380.0)] {
            let layout = compute(size(w, 400.0), open, 3, false, 0.0);
            assert_eq!(
                layout.strip.x + layout.strip.width,
                w,
                "strip flush with the bar's right edge (open = {open})"
            );
            assert_eq!(
                layout.handle.x + layout.handle.width,
                w,
                "and so is the grip"
            );
            for item in &layout.items {
                assert_eq!(item.x + item.width, w, "items ride with the strip");
            }
        }
    }

    /// Y-up: item 0 is the *topmost*, i.e. the one with the highest Y.
    #[test]
    fn strip_items_stack_downward_from_the_top() {
        let layout = compute(size(COLLAPSED_W, 400.0), false, 3, false, 0.0);
        assert!(layout.items[0].y > layout.items[1].y);
        assert!(layout.items[1].y > layout.items[2].y);
        assert_eq!(layout.items[0].y + layout.items[0].height, 400.0 - PAD);
    }

    /// A palette taller than the strip is *scrolled*, not truncated
    /// (ND's `overflow-y: auto`): every favourite has a rectangle, and
    /// which of them are on screen is the viewport's business.
    #[test]
    fn a_long_palette_scrolls_instead_of_dropping_items() {
        let available = size(COLLAPSED_W, 3.0 * ITEM_H + PAD * 2.0);
        let layout = compute(available, false, 10, false, 0.0);
        assert_eq!(layout.items.len(), 10, "every favourite is placed");
        let visible = |l: &BarLayout| {
            l.items
                .iter()
                .filter(|r| item_visible(**r, l.items_viewport))
                .count()
        };
        assert_eq!(visible(&layout), 3, "three of them fit at rest");
        assert!(
            !item_visible(layout.items[9], layout.items_viewport),
            "the last one starts off-screen"
        );

        let max = max_scroll(available, 10, false);
        assert!((max - (10.0 * ITEM_H - layout.items_viewport.height)).abs() < 1e-9);
        let scrolled = compute(available, false, 10, false, max);
        assert!(
            item_visible(scrolled.items[9], scrolled.items_viewport),
            "scrolled to the end, the last favourite is reachable"
        );
        assert!(
            (scrolled.items[9].y - scrolled.items_viewport.y).abs() < 1e-9,
            "and sits flush with the bottom of the viewport"
        );
    }

    /// A palette that fits does not scroll at all.
    #[test]
    fn a_short_palette_has_no_scroll_range() {
        assert_eq!(max_scroll(size(COLLAPSED_W, 400.0), 3, false), 0.0);
    }

    /// The pin affordance is anchored to the strip's bottom, outside the
    /// scroll region — it must not scroll away.
    #[test]
    fn pin_item_is_anchored_below_the_scroll_region() {
        let h = 3.0 * ITEM_H + PAD * 2.0;
        let available = size(COLLAPSED_W, h);
        let layout = compute(available, false, 3, true, 0.0);
        let pin = layout.pin.expect("the pin item keeps its slot");
        assert_eq!(pin.y, PAD, "anchored to the strip's bottom");
        assert!(
            layout.items_viewport.y >= pin.y + pin.height,
            "and the items scroll above it"
        );
        // Reserving the pin's slot costs the list one visible row.
        let max = max_scroll(available, 3, true);
        assert!(max > 0.0, "three items no longer fit beside the pin");
        let scrolled = compute(available, false, 3, true, max);
        assert!(item_visible(scrolled.items[2], scrolled.items_viewport));
        assert_eq!(
            scrolled.pin.expect("the pin stays put").y,
            PAD,
            "scrolling the items must not move the pin"
        );
    }

    /// The icon slot is the ND tile, centered in the strip, with the
    /// label line under it.
    #[test]
    fn item_carves_into_a_slot_and_a_label_line() {
        let layout = compute(size(COLLAPSED_W, 400.0), false, 1, false, 0.0);
        let item = layout.items[0];
        let slot = icon_slot(item);
        assert_eq!(slot.width, ICON_SLOT);
        assert_eq!(slot.height, ICON_SLOT);
        assert!((slot.x - item.x - (STRIP_W - ICON_SLOT) * 0.5).abs() < 1e-9);
        let label = label_box(item);
        assert!(
            label.y + label.height <= slot.y,
            "the label sits under the slot"
        );
        assert!(label.y >= item.y);
    }

    /// The handle *is* the 16 × 56 grip, vertically centered — nothing
    /// above or below it belongs to the handle, which is what lets a
    /// press there reach the strip item underneath.
    #[test]
    fn the_handle_is_only_the_centered_grip() {
        let layout = compute(size(COLLAPSED_W, 400.0), false, 4, false, 0.0);
        let grip = layout.handle;
        assert_eq!(grip.width, HANDLE_W);
        assert_eq!(grip.height, HANDLE_H);
        assert!((grip.y + grip.height * 0.5 - 200.0).abs() < 1e-9);
        // A point on the same column but well above the grip is not the
        // handle — it is the strip.
        let above = Point::new(grip.x + grip.width * 0.5, grip.y + grip.height + 20.0);
        assert!(!grip.contains(above));
        assert!(layout.strip.contains(above), "the strip is underneath");
        let below = Point::new(grip.x + grip.width * 0.5, grip.y - 20.0);
        assert!(!grip.contains(below));
        assert!(layout.strip.contains(below));
    }

    /// Degenerate sizes stay non-negative — the widget paints and
    /// hit-tests them without special cases.
    #[test]
    fn zero_size_bar_has_no_negative_rects() {
        let layout = compute(size(0.0, 0.0), true, 5, true, 0.0);
        assert!(layout.strip.width >= 0.0 && layout.strip.height >= 0.0);
        assert!(layout.pin.is_none());
        assert_eq!(layout.items_viewport.height, 0.0);
        assert!(
            layout
                .items
                .iter()
                .all(|r| !item_visible(*r, layout.items_viewport)),
            "nothing is reachable in a zero-sized bar"
        );
        assert_eq!(layout.panel.map(|r| r.width), Some(0.0));
        assert_eq!(max_scroll(size(0.0, 0.0), 5, true), 5.0 * ITEM_H);
    }
}
