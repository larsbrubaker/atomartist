//! The floating "ghost" that follows the cursor while a favorites-bar /
//! browser item is being dragged toward the node canvas
//! (`docs/file-browser-design.md` §2 drop-pipeline row, §7 open question 1).
//!
//! Design question 1 ("agg-gui overlay layer vs a floating widget") is
//! resolved in favour of a floating widget: the ghost is handed to the
//! app shell's [`FloatingOverlayHost`](crate::floating_overlay) — the
//! same top-of-`Stack` slot the colour picker uses — so it can be drawn
//! anywhere in the window without a new agg-gui layer.
//!
//! The widget positions *itself*: the host preserves whatever bounds a
//! child reports after `layout`, so the ghost reads the app-level cursor
//! (`agg_gui::widget::current_mouse_world`, the same channel nested
//! drags use) and parks its rectangle next to it. That keeps the
//! controller in [`crate::drag_insert`] free of any window-space
//! geometry: it only decides *whether* a ghost exists.
//!
//! Coordinates are agg-gui's bottom-up Y: the ghost's `Rect::y` is its
//! **bottom** edge, and it is offset *below* the cursor (smaller Y) so
//! the pointer stays visible above the label.

use std::cell::Cell;
use std::rc::Rc;

use agg_gui::{
    theme::current_visuals, DrawCtx, Event, EventResult, HAnchor, Insets, Point, Rect, Size,
    VAnchor, Widget, WidgetBase,
};

/// Height of the ghost pill.
const GHOST_H: f64 = 26.0;
/// Padding inside the pill, and the gap between glyph and label.
const PAD: f64 = 8.0;
/// Label / glyph point size.
const TEXT_SIZE: f64 = 12.0;
/// How far the pill's top-left sits from the cursor. NodeDesigner
/// centres its ghost on the pointer; we offset instead so the cursor
/// (and whatever it is about to hit) is never covered.
const CURSOR_OFFSET: f64 = 10.0;

/// Cursor-following drag ghost. One per gesture; dropped by setting the
/// close flag the controller kept.
pub struct DragGhost {
    glyph: char,
    label: String,
    bounds: Rect,
    base: WidgetBase,
    children: Vec<Box<dyn Widget>>,
    /// Shared with [`crate::drag_insert`]; set to `true` to make the
    /// overlay host drop this widget on its next pass.
    close: Rc<Cell<bool>>,
}

impl DragGhost {
    pub fn new(glyph: char, label: impl Into<String>, close: Rc<Cell<bool>>) -> Self {
        Self {
            glyph,
            label: label.into(),
            bounds: Rect::default(),
            base: WidgetBase::new()
                .with_h_anchor(HAnchor::LEFT)
                .with_v_anchor(VAnchor::BOTTOM),
            children: Vec::new(),
            close,
        }
    }

    /// Width the pill needs for its glyph + label.
    fn desired_width(&self) -> f64 {
        let text = crate::file_browser::widget_geom::measure(&self.label, TEXT_SIZE);
        PAD + TEXT_SIZE + PAD + text + PAD
    }

    /// Rectangle for a cursor at `cursor` (window / world coords, Y-up),
    /// clamped so the pill never leaves the window.
    pub fn rect_for_cursor(&self, cursor: Point, window: Size) -> Rect {
        let w = self.desired_width();
        // Y-up: "below and right of the cursor" is +x, -y.
        let x = cursor.x + CURSOR_OFFSET;
        let y = cursor.y - CURSOR_OFFSET - GHOST_H;
        let x = if window.width > w {
            x.clamp(0.0, window.width - w)
        } else {
            0.0
        };
        let y = if window.height > GHOST_H {
            y.clamp(0.0, window.height - GHOST_H)
        } else {
            0.0
        };
        Rect::new(x, y, w, GHOST_H)
    }
}

impl Widget for DragGhost {
    fn type_name(&self) -> &'static str {
        "DragGhost"
    }
    fn id(&self) -> Option<&str> {
        Some(GHOST_ID)
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

    /// Track the cursor. The overlay host keeps whatever bounds we
    /// report here, so this is the ghost's whole positioning story.
    fn layout(&mut self, available: Size) -> Size {
        if let Some(cursor) = agg_gui::widget::current_mouse_world() {
            self.bounds = self.rect_for_cursor(cursor, available);
        }
        Size::new(self.bounds.width, self.bounds.height)
    }

    fn paint(&mut self, ctx: &mut dyn DrawCtx) {
        let visuals = current_visuals();
        let w = self.bounds.width;
        let h = self.bounds.height;
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        if let Some(font) = agg_gui::font_settings::current_system_font() {
            ctx.set_font(font);
        }
        ctx.set_global_alpha(0.85);
        ctx.set_fill_color(visuals.panel_fill);
        ctx.begin_path();
        ctx.rounded_rect(0.0, 0.0, w, h, 4.0);
        ctx.fill();
        ctx.set_stroke_color(visuals.separator);
        ctx.set_line_width(1.0);
        ctx.begin_path();
        ctx.rounded_rect(0.5, 0.5, w - 1.0, h - 1.0, 4.0);
        ctx.stroke();

        ctx.set_fill_color(visuals.text_color);
        ctx.set_font_size(TEXT_SIZE);
        let baseline = (h - TEXT_SIZE) * 0.5 + 1.0;
        ctx.fill_text(&self.glyph.to_string(), PAD, baseline);
        ctx.fill_text(&self.label, PAD + TEXT_SIZE + PAD, baseline);
        ctx.set_global_alpha(1.0);
    }

    /// The ghost is decoration: it never takes input. The gesture that
    /// spawned it holds the mouse capture.
    fn hit_test(&self, _local_pos: Point) -> bool {
        false
    }

    fn on_event(&mut self, _event: &Event) -> EventResult {
        EventResult::Ignored
    }

    fn properties(&self) -> Vec<(&'static str, String)> {
        vec![
            ("label", self.label.clone()),
            ("closing", self.close.get().to_string()),
        ]
    }
}

/// Widget id the harness looks the ghost up by (design §6).
pub const GHOST_ID: &str = "drag-ghost";

#[cfg(test)]
mod tests {
    use super::*;

    fn ghost() -> DragGhost {
        DragGhost::new('X', "Box", Rc::new(Cell::new(false)))
    }

    /// Y-up: the pill hangs *below* the cursor, so its top edge is under
    /// the pointer rather than over it.
    #[test]
    fn ghost_sits_below_and_right_of_the_cursor() {
        let g = ghost();
        let r = g.rect_for_cursor(Point::new(400.0, 300.0), Size::new(1280.0, 720.0));
        assert!(r.x > 400.0, "ghost should trail to the right of the cursor");
        assert!(
            r.y + r.height < 300.0,
            "ghost top edge must be below the cursor in Y-up coords"
        );
    }

    /// A cursor near the window edge must not push the pill off-screen.
    #[test]
    fn ghost_clamps_inside_the_window() {
        let g = ghost();
        let window = Size::new(1280.0, 720.0);
        let r = g.rect_for_cursor(Point::new(1279.0, 2.0), window);
        assert!(r.x >= 0.0 && r.x + r.width <= window.width);
        assert!(r.y >= 0.0 && r.y + r.height <= window.height);
    }
}
