//! Pointer handling for [`crate::favorites_bar::FavoritesBar`].
//!
//! Split out of `favorites_bar.rs` (state + assembly) and
//! `favorites_bar_paint.rs` (drawing) so none of the three approaches the
//! 800-line cap. This file owns everything the bar does with a mouse:
//! the handle press/drag/release that toggles and resizes the browser
//! panel, the strip's item press → drag-insert → click-activate chain,
//! the wheel scroll, and the grip's hover tracking.
//!
//! Declared as a **child** module of `favorites_bar` (via `#[path]`)
//! rather than a sibling in `lib.rs`: these handlers mutate the bar's
//! private fields directly, and a child module sees them without having
//! to widen every field to `pub(crate)`.
//!
//! Coordinates are bar-local and Y-up; every rectangle hit-tested here
//! comes from [`crate::favorites_bar_geom`].
//!
//! # Hover is only the grip
//!
//! Since 6g-2 the handle reserves no width — it is a 16 × 56 tab
//! floating on the strip's edge. Its hover highlight and its tooltip
//! therefore key off the grip rectangle alone, never the bar or the
//! strip. agg-gui delivers a `MouseMove` at `(-1, -1)` to the widget the
//! pointer just left (`App::dispatch_mouse_move`), which no rectangle
//! contains, so the same containment test both sets and clears the flag.
//!
//! That covers the pointer moving *within* the window. Leaving the
//! **window** produces no `CursorMoved` at all, so the shells have to
//! say so explicitly or a fast flick out latches the highlight: both
//! forward it to `App::on_mouse_leave`, which re-dispatches the same
//! `(-1, -1)` sentinel (`demo-native`'s `WindowEvent::CursorLeft`,
//! `demo-wasm`'s canvas `mouseleave` listener → its `on_mouse_leave`
//! export). This is the app's first hover-latching widget, which is why
//! that wiring landed with it.

use agg_gui::{EventResult, Point, Size};

use crate::drag_insert::GestureEnd;
use crate::favorites_bar::{FavoritesBar, SCROLL_STEP};
use crate::favorites_bar_geom as geom;
use crate::favorites_bar_handle::{clamp_panel, HandleGesture};
use crate::favorites_strip::StripItem;

/// Tooltip shown over the grip while the browser panel is closed —
/// `parts-bar.js`'s `handle.title`.
const SHOW_TOOLTIP: &str = "Show library (drag to resize)";
/// …and while it is open.
const HIDE_TOOLTIP: &str = "Hide library (drag to resize)";

impl FavoritesBar {
    /// Whether the grip should be drawn in its highlighted state: the
    /// pointer is over it, **or** a resize is in flight and has dragged
    /// the pointer off it. Read by the paint module and published as a
    /// property.
    pub(crate) fn handle_hovered(&self) -> bool {
        self.handle_hovered || self.drag.is_some()
    }

    /// Adopt `hovered` for the grip, redrawing and re-publishing the
    /// tooltip when it actually changes. Hover state is the textbook
    /// "mutated in a `MouseMove` that returns `Ignored`" case, so the
    /// explicit `request_draw` is what makes it visible at all (see
    /// `Widget::on_event`'s invalidation contract).
    fn set_handle_hovered(&mut self, hovered: bool) {
        if self.handle_hovered == hovered {
            return;
        }
        self.handle_hovered = hovered;
        self.sync_handle_tooltip();
        agg_gui::animation::request_draw();
    }

    /// Publish (or withdraw) the grip's hover help on the bar's
    /// `WidgetBase`, which is the app-wide tooltip controller's input.
    ///
    /// The bar is one widget, so the text has to appear only while the
    /// pointer is over the grip — otherwise hovering a strip item would
    /// offer "Show library". Hence `Some` while hovered, `None`
    /// otherwise, rather than a constant string set once at construction.
    pub(crate) fn sync_handle_tooltip(&mut self) {
        let text = self.handle_hovered.then(|| {
            if self.expanded() {
                HIDE_TOOLTIP.to_string()
            } else {
                SHOW_TOOLTIP.to_string()
            }
        });
        self.base.tooltip = text;
    }

    pub(super) fn on_mouse_down(&mut self, pos: Point) -> EventResult {
        if self.layout.handle.contains(pos) {
            // A press that starts a *different* gesture takes over the
            // mouse capture, so the drag-insert in flight would never
            // see its release: end it here rather than orphan whatever
            // it was carrying.
            if let Some(insert) = self.insert.clone() {
                insert.cancel();
            }
            self.pressed_item = None;
            self.drag = Some(HandleGesture::begin(pos.x, self.panel_width()));
            return EventResult::Consumed;
        }
        if let Some(index) = self.item_at(pos) {
            // Activation is deferred to the release: this press may
            // still turn into a drag, and a drag must not *also* open
            // the project it was carrying.
            self.pressed_item = self
                .items
                .get(index)
                .map(|item| (item.kind, item.stable_key.clone()));
            let payload = self.items.get(index).and_then(StripItem::payload);
            if let (Some(insert), Some(payload)) = (self.insert.clone(), payload) {
                insert.press(payload, pos);
            }
            return EventResult::Consumed;
        }
        if self.layout.pin.is_some_and(|rect| rect.contains(pos)) {
            self.pin_current_project();
            return EventResult::Consumed;
        }
        // The bar is opaque chrome: a click on its background must not
        // fall through to whatever is behind it.
        EventResult::Consumed
    }

    /// Wheel over the strip scrolls the favourites (ND's
    /// `overflow-y: auto`). agg-gui's sign convention: positive
    /// `delta_y` means "show me what is above", i.e. *decrease* the
    /// offset. The clamp is re-applied every layout, so a wheel spun
    /// against a short palette does nothing at all.
    pub(super) fn on_wheel(&mut self, pos: Point, delta_y: f64) -> EventResult {
        if !self.layout.strip.contains(pos) || self.max_scroll() <= 0.0 {
            return EventResult::Ignored;
        }
        let next = (self.scroll - delta_y * SCROLL_STEP).clamp(0.0, self.max_scroll());
        if next != self.scroll {
            self.scroll = next;
            agg_gui::animation::request_draw();
        }
        EventResult::Consumed
    }

    /// Furthest the strip may scroll at the size it was last laid out
    /// at. Zero whenever every favourite already fits.
    pub(super) fn max_scroll(&self) -> f64 {
        geom::max_scroll(
            Size::new(self.bounds.width, self.bounds.height),
            self.items.len(),
            self.show_pin,
        )
    }

    pub(super) fn on_mouse_move(&mut self, pos: Point) -> EventResult {
        self.set_handle_hovered(self.layout.handle.contains(pos));
        let Some(drag) = self.drag.as_mut() else {
            // No handle gesture — the press may instead be a
            // drag-insert (a favourite on its way to the canvas).
            if let Some(insert) = self.insert.clone() {
                if insert.pointer_move(pos) {
                    return EventResult::Consumed;
                }
            }
            return EventResult::Ignored;
        };
        if !drag.pointer_x(pos.x) {
            // Still inside the toggle threshold: the release will be a
            // click, and there is nothing to redraw.
            return EventResult::Consumed;
        }
        // Pull-open: a rightward drag out of the collapsed bar opens the
        // panel as it sizes. The *width* is deliberately not committed
        // here — see [`FavoritesBar::on_mouse_up`]. Mid-drag the panel
        // renders the gesture's raw width anyway, so the user still sees
        // it follow the pointer.
        if drag.wants_open() {
            self.set_expanded(true);
        }
        agg_gui::animation::request_draw();
        EventResult::Consumed
    }

    /// End of the gesture — and the only place the stored width moves.
    ///
    /// Committing on each `MouseMove` looks equivalent and is not: a drag
    /// that closes the panel sweeps through every width on its way down,
    /// so the last one above the threshold would be written just before
    /// the release. "Snap closed keeps the stored width" would then mean
    /// "keeps ≈120 px", not the width the user actually sized to. So a
    /// released-narrow gesture writes nothing at all and the previous
    /// size stands.
    pub(super) fn on_mouse_up(&mut self, pos: Point) -> EventResult {
        let Some(drag) = self.drag.take() else {
            // Drag-insert release: a sub-threshold press is still the
            // item's click, anything else was handled by the controller.
            let pressed = self.pressed_item.take();
            if let Some(insert) = self.insert.clone() {
                match insert.pointer_up(pos) {
                    GestureEnd::Click => {
                        if let Some(key) = pressed {
                            self.activate_item(&key);
                        }
                        return EventResult::Consumed;
                    }
                    GestureEnd::Dropped | GestureEnd::Cancelled => {
                        agg_gui::animation::request_draw();
                        return EventResult::Consumed;
                    }
                    GestureEnd::None => {}
                }
            }
            // No controller attached: keep the pre-drag-insert behaviour
            // of activating the pressed item.
            if let Some(key) = pressed {
                self.activate_item(&key);
                return EventResult::Consumed;
            }
            return EventResult::Ignored;
        };
        if !drag.is_resizing() {
            // A press released in place is the toggle.
            let expanded = self.expanded();
            self.set_expanded(!expanded);
        } else if drag.wants_open() {
            self.set_expanded(true);
            self.set_stored_width(clamp_panel(drag.raw(), self.max_panel_width()));
        } else {
            // Snap closed — and keep the stored width, so the next open
            // is the size the user had chosen.
            self.set_expanded(false);
        }
        // The toggle just flipped Show ⇄ Hide; the pointer is still on
        // the grip, so the tip has to follow.
        self.sync_handle_tooltip();
        agg_gui::animation::request_draw();
        EventResult::Consumed
    }
}
