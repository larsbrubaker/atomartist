//! The channel that tells [`FavoritesBar`](crate::favorites_bar::FavoritesBar)
//! how big — and where — the panes around it actually are.
//!
//! Two consumers need this:
//!
//! 1. The bar caps its panel at
//!    [`MAX_WIDTH_FRACTION`](crate::favorites_bar::MAX_WIDTH_FRACTION) of
//!    its host pane, and a widget cannot read its parent's size:
//!    `layout` is handed a slot, not a context. Worse, the slot is not
//!    even a stable question — `FlexRow` lays a fixed child out
//!    **twice**, once with the row's inner width (to measure it) and once
//!    with the width that measurement produced (to place it). A bar that
//!    treated `available.width` as "the host" would feed its own width
//!    back into the cap and shrink 40 % per frame.
//!
//!    Guessing which call is which by comparing widths *nearly* works and
//!    fails exactly where it matters: when the pane happens to be as wide
//!    as the bar, every measure pass looks like the echo, the cap freezes
//!    at whatever it last was, and the bar can take the whole pane.
//!
//! 2. Since step 6f-1 the bar lives in the **3-D viewport pane** while
//!    its drag-insert drop target is the **node canvas** in the *other*
//!    pane of the splitter. The controller speaks bar-local coordinates,
//!    so it needs the canvas pane's rectangle expressed relative to the
//!    bar's own pane. A [`PaneRectProbe`] wrapping each pane records the
//!    bounds the `Splitter` placed it at — both in the splitter's
//!    coordinate space — and the difference is exactly that offset. No
//!    divider-thickness constant is duplicated here as a result.
//!
//! So the geometry is published explicitly. [`PaneRectProbe`] wraps a
//! pane's content, records the size it is given (before any flex
//! splitting happens) and the bounds it is placed at, and consumers read
//! it out of the shared [`PaneRect`]. One writer, one reader, no
//! inference.
//!
//! # The published *position* is one frame behind
//!
//! `layout` runs top-down and `set_bounds` follows it, so a pane's
//! **size** is this frame's but its **position** is whatever the parent
//! last placed it at — during the very first layout, all zeros. That is
//! deliberate and safe for both consumers: the width cap only reads the
//! size, and the drag controller reads the position at *event* time,
//! after at least one full layout+placement pass has run. The only
//! window where it could bite is a frame in which the splitter ratio or
//! the window height changes *and* a drag is released, which would land
//! the drop against the previous frame's canvas rectangle — a
//! sub-pixel-to-a-few-pixel error in a gesture the user cannot perform
//! without letting go of the splitter first.

use std::cell::Cell;
use std::rc::Rc;

use agg_gui::{
    DrawCtx, Event, EventResult, HAnchor, Insets, Rect, Size, VAnchor, Widget, WidgetBase,
};

/// Shared "where and how big is this pane" cell. Cheap to clone; both
/// ends live on the UI thread inside the widget tree.
#[derive(Clone, Default)]
pub struct PaneRect(Rc<Cell<Rect>>);

impl PaneRect {
    pub fn new() -> Self {
        PaneRect(Rc::new(Cell::new(Rect::default())))
    }

    /// Last rectangle published by the probe, in the coordinate space of
    /// the probe's parent. All zeros before the first layout — callers
    /// treat that as "unknown", not as "no room".
    pub fn get(&self) -> Rect {
        self.0.get()
    }

    /// Convenience for the width-cap consumer.
    pub fn width(&self) -> f64 {
        self.0.get().width
    }

    /// Publish the pane's size, keeping the last known position. Called
    /// from `layout`, which runs *before* the parent places the pane.
    pub fn set_size(&self, size: Size) {
        let old = self.0.get();
        self.0.set(Rect::new(old.x, old.y, size.width, size.height));
    }

    /// Publish the full rectangle. Called from `set_bounds`, where the
    /// parent has finally said where the pane goes.
    pub fn set_rect(&self, rect: Rect) {
        self.0.set(rect);
    }
}

/// The node canvas's rectangle in **bar-local** coordinates, or `None`
/// before both pane probes have published. The bar's origin *is* its
/// pane's origin (it is the pane row's first, left-anchored child), so
/// the offset is simply the difference between the two pane rectangles.
///
/// This is the rectangle [`DragInsertHandle::set_canvas_rect`](
/// crate::drag_insert::DragInsertHandle::set_canvas_rect) is fed.
pub fn canvas_rect_local(pane: Rect, canvas: Rect) -> Option<Rect> {
    if pane.width <= 0.0 || canvas.width <= 0.0 || canvas.height <= 0.0 {
        return None;
    }
    Some(Rect::new(
        canvas.x - pane.x,
        canvas.y - pane.y,
        canvas.width,
        canvas.height,
    ))
}

/// The 3-D viewport's rectangle in **bar-local** coordinates: whatever
/// is left of the bar's own pane once the bar (strip + panel + handle,
/// i.e. `bar_width`) has taken its share. `None` before the pane probe
/// has published, or when nothing is left beside the bar.
///
/// Starting the rectangle at the bar's right edge is what makes a
/// release over the bar's own chrome a *cancel* rather than a bed drop
/// (design §5b, step 6f-4).
///
/// **Accepted v1 limitation:** the rectangle is the whole viewport,
/// *including* the overlay chrome drawn on top of it (the HUD bay's
/// buttons, the view gizmo). A drop on one of those reads as a drop on
/// the bed. NodeDesigner excluded overlays by hit-testing the DOM
/// (`elementFromPoint`); doing the equivalent here means asking the
/// widget tree what is under the cursor, which is a follow-up.
pub fn viewport_rect_local(pane: Rect, bar_width: f64) -> Option<Rect> {
    if pane.width <= 0.0 || pane.height <= 0.0 {
        return None;
    }
    let width = pane.width - bar_width;
    if width <= 0.0 {
        return None;
    }
    Some(Rect::new(bar_width, 0.0, width, pane.height))
}

/// Pass-through widget that publishes its own layout size and placement
/// into a [`PaneRect`] and then lays its single child out in the same
/// slot.
///
/// Deliberately inert otherwise: it paints nothing, consumes nothing, and
/// stretches, so wrapping content in it changes no geometry.
pub struct PaneRectProbe {
    rect: PaneRect,
    children: Vec<Box<dyn Widget>>,
    bounds: Rect,
    base: WidgetBase,
}

impl PaneRectProbe {
    pub fn new(rect: PaneRect, child: Box<dyn Widget>) -> Self {
        PaneRectProbe {
            rect,
            children: vec![child],
            bounds: Rect::default(),
            base: WidgetBase::new()
                .with_h_anchor(HAnchor::STRETCH)
                .with_v_anchor(VAnchor::STRETCH),
        }
    }
}

impl Widget for PaneRectProbe {
    fn type_name(&self) -> &'static str {
        "PaneRectProbe"
    }
    fn bounds(&self) -> Rect {
        self.bounds
    }
    fn set_bounds(&mut self, b: Rect) {
        self.bounds = b;
        // Where the parent put us — the half `layout` cannot know.
        self.rect.set_rect(b);
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
        // during its own layout sees this frame's size.
        self.rect.set_size(available);
        self.bounds = Rect::new(
            self.bounds.x,
            self.bounds.y,
            available.width,
            available.height,
        );
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
        let rect = PaneRect::new();
        assert_eq!(rect.width(), 0.0, "unknown before the first layout");
        let mut probe = PaneRectProbe::new(rect.clone(), Box::new(Spacer::new()));

        probe.layout(Size::new(1280.0, 400.0));
        assert_eq!(rect.width(), 1280.0);
        // A pane that shrinks to a width the consumer previously reported
        // is still published — the case the old width-comparison
        // heuristic mistook for its parent's echo.
        probe.layout(Size::new(400.0, 400.0));
        assert_eq!(rect.width(), 400.0);
        probe.layout(Size::new(400.0, 400.0));
        assert_eq!(rect.width(), 400.0);
    }

    /// Placement comes from `set_bounds`, which is what makes two probes
    /// comparable: both report in their common parent's space.
    #[test]
    fn probe_publishes_its_placement() {
        let rect = PaneRect::new();
        let mut probe = PaneRectProbe::new(rect.clone(), Box::new(Spacer::new()));
        probe.layout(Size::new(1280.0, 400.0));
        probe.set_bounds(Rect::new(0.0, 286.0, 1280.0, 400.0));
        assert_eq!(rect.get(), Rect::new(0.0, 286.0, 1280.0, 400.0));
        // A re-layout keeps the placement until the parent moves us.
        probe.layout(Size::new(1280.0, 300.0));
        assert_eq!(rect.get(), Rect::new(0.0, 286.0, 1280.0, 300.0));
    }

    /// The viewport rectangle starts where the bar ends, so the bar's own
    /// strip / panel / handle are never inside it — that is what keeps a
    /// release over the chrome a cancel rather than a bed drop.
    #[test]
    fn viewport_rect_excludes_the_bar() {
        let pane = Rect::new(0.0, 286.0, 1280.0, 400.0);
        let rect = viewport_rect_local(pane, 88.0).expect("room beside the bar");
        assert_eq!(rect, Rect::new(88.0, 0.0, 1192.0, 400.0));
        assert!(
            !rect.contains(agg_gui::Point::new(40.0, 200.0)),
            "the strip"
        );
        assert!(
            !rect.contains(agg_gui::Point::new(87.0, 200.0)),
            "the handle"
        );
        assert!(rect.contains(agg_gui::Point::new(600.0, 200.0)), "the bed");
        // Unknown pane, or a bar that fills it: no drop target at all.
        assert!(viewport_rect_local(Rect::default(), 88.0).is_none());
        assert!(viewport_rect_local(pane, 1280.0).is_none());
    }

    /// The canvas rectangle is the *other* pane expressed relative to the
    /// bar's pane — below it, hence a negative `y`, in the 6f-1 shape.
    #[test]
    fn canvas_rect_is_relative_to_the_bars_pane() {
        let viewport = Rect::new(0.0, 286.0, 1280.0, 400.0);
        let canvas = Rect::new(0.0, 80.0, 1280.0, 200.0);
        let rect = canvas_rect_local(viewport, canvas).expect("both panes published");
        assert_eq!(rect, Rect::new(0.0, -206.0, 1280.0, 200.0));
        assert!(canvas_rect_local(Rect::default(), canvas).is_none());
        assert!(canvas_rect_local(viewport, Rect::default()).is_none());
    }

    /// The child is laid out in the same slot, so wrapping content in the
    /// probe changes no geometry.
    #[test]
    fn probe_passes_the_slot_through_untouched() {
        let mut probe = PaneRectProbe::new(PaneRect::new(), Box::new(Spacer::new()));
        let reported = probe.layout(Size::new(640.0, 480.0));
        assert_eq!(reported, Size::new(640.0, 480.0));
        assert_eq!(
            probe.children()[0].bounds(),
            Rect::new(0.0, 0.0, 640.0, 480.0)
        );
    }
}
