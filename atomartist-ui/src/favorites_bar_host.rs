//! The channel that tells [`FavoritesBar`](crate::favorites_bar::FavoritesBar)
//! how wide the pane it is docked in actually is.
//!
//! The bar caps itself at
//! [`MAX_WIDTH_FRACTION`](crate::favorites_bar::MAX_WIDTH_FRACTION) of its
//! host, and a widget cannot read its parent's size: `layout` is handed a
//! slot, not a context. Worse, the slot is not even a stable question —
//! `FlexRow` lays a fixed child out **twice**, once with the row's inner
//! width (to measure it) and once with the width that measurement produced
//! (to place it). A bar that treated `available.width` as "the host" would
//! feed its own width back into the cap and shrink 40 % per frame.
//!
//! Guessing which call is which by comparing widths *nearly* works and
//! fails exactly where it matters: when the pane happens to be as wide as
//! the bar, every measure pass looks like the echo, the cap freezes at
//! whatever it last was, and the bar can take the whole pane — leaving a
//! zero-width node canvas until the next resize.
//!
//! So the width is published explicitly instead. [`PaneWidthProbe`] wraps
//! the pane's content, records the size it is given before any flex
//! splitting happens, and the bar reads it out of the shared
//! [`PaneWidth`]. One writer, one reader, no inference.

use std::cell::Cell;
use std::rc::Rc;

use agg_gui::{
    DrawCtx, Event, EventResult, HAnchor, Insets, Rect, Size, VAnchor, Widget, WidgetBase,
};

/// Shared "how wide is the pane" cell. Cheap to clone; both ends live on
/// the UI thread inside the widget tree.
#[derive(Clone, Default)]
pub struct PaneWidth(Rc<Cell<f64>>);

impl PaneWidth {
    pub fn new() -> Self {
        PaneWidth(Rc::new(Cell::new(0.0)))
    }

    /// Last width published by the probe. `0.0` before the first layout —
    /// callers treat that as "unknown", not as "no room".
    pub fn get(&self) -> f64 {
        self.0.get()
    }

    pub fn set(&self, width: f64) {
        self.0.set(width);
    }
}

/// Pass-through widget that publishes its own layout width into a
/// [`PaneWidth`] and then lays its single child out in the same slot.
///
/// Deliberately inert otherwise: it paints nothing, consumes nothing, and
/// stretches, so wrapping content in it changes no geometry.
pub struct PaneWidthProbe {
    width: PaneWidth,
    children: Vec<Box<dyn Widget>>,
    bounds: Rect,
    base: WidgetBase,
}

impl PaneWidthProbe {
    pub fn new(width: PaneWidth, child: Box<dyn Widget>) -> Self {
        PaneWidthProbe {
            width,
            children: vec![child],
            bounds: Rect::default(),
            base: WidgetBase::new()
                .with_h_anchor(HAnchor::STRETCH)
                .with_v_anchor(VAnchor::STRETCH),
        }
    }
}

impl Widget for PaneWidthProbe {
    fn type_name(&self) -> &'static str {
        "PaneWidthProbe"
    }
    fn bounds(&self) -> Rect {
        self.bounds
    }
    fn set_bounds(&mut self, b: Rect) {
        self.bounds = b;
    }
    fn children(&self) -> &[Box<dyn Widget>] {
        &self.children
    }
    fn children_mut(&mut self) -> &mut Vec<Box<dyn Widget>> {
        &mut self.children
    }
    fn margin(&self) -> Insets {
        Insets::ZERO
    }
    fn h_anchor(&self) -> HAnchor {
        self.base.h_anchor
    }
    fn v_anchor(&self) -> VAnchor {
        self.base.v_anchor
    }
    fn widget_base(&self) -> Option<&WidgetBase> {
        Some(&self.base)
    }
    fn widget_base_mut(&mut self) -> Option<&mut WidgetBase> {
        Some(&mut self.base)
    }

    fn layout(&mut self, available: Size) -> Size {
        // Published *before* the child runs, so a descendant reading it
        // during its own layout sees this frame's value.
        self.width.set(available.width);
        self.bounds = Rect::new(0.0, 0.0, available.width, available.height);
        if let Some(child) = self.children.first_mut() {
            child.layout(available);
            child.set_bounds(Rect::new(0.0, 0.0, available.width, available.height));
        }
        available
    }

    fn paint(&mut self, _ctx: &mut dyn DrawCtx) {}

    fn on_event(&mut self, _event: &Event) -> EventResult {
        EventResult::Ignored
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agg_gui::Spacer;

    /// The probe publishes what it is given, every time — no filtering,
    /// which is the whole point of having an explicit channel.
    #[test]
    fn probe_publishes_every_layout_width() {
        let width = PaneWidth::new();
        assert_eq!(width.get(), 0.0, "unknown before the first layout");
        let mut probe = PaneWidthProbe::new(width.clone(), Box::new(Spacer::new()));

        probe.layout(Size::new(1280.0, 400.0));
        assert_eq!(width.get(), 1280.0);
        // A pane that shrinks to a width the consumer previously reported
        // is still published — the case the old width-comparison
        // heuristic mistook for its parent's echo.
        probe.layout(Size::new(400.0, 400.0));
        assert_eq!(width.get(), 400.0);
        probe.layout(Size::new(400.0, 400.0));
        assert_eq!(width.get(), 400.0);
    }

    /// The child is laid out in the same slot, so wrapping content in the
    /// probe changes no geometry.
    #[test]
    fn probe_passes_the_slot_through_untouched() {
        let mut probe = PaneWidthProbe::new(PaneWidth::new(), Box::new(Spacer::new()));
        let reported = probe.layout(Size::new(640.0, 480.0));
        assert_eq!(reported, Size::new(640.0, 480.0));
        assert_eq!(
            probe.children()[0].bounds(),
            Rect::new(0.0, 0.0, 640.0, 480.0)
        );
    }
}
