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
//! # Two looks: the rendered icon, or a labelled pill
//!
//! NodeDesigner's ghost (`parts-bar-drag.js`, `.nd-parts-drag-ghost`) is
//! a 48 × 48 element centred on the pointer at 85 % opacity, holding the
//! *item's cached icon image* — with the title text as the fallback when
//! no icon rendered. Step 6g-3 gives us the same: a node-type drag whose
//! [`crate::node_icons`] render is available carries that image; a file
//! payload, an un-rendered type, or a backend with no image blit keeps
//! the glyph-and-label pill this widget started as.
//!
//! Coordinates are agg-gui's bottom-up Y. The icon ghost is *centred* on
//! the cursor (the ancestor's `−GHOST_SIZE/2` on both axes, which needs
//! no Y flip because it is symmetric); the pill hangs *below* the cursor
//! (smaller Y) so the pointer stays visible above the label.

use std::cell::Cell;
use std::rc::Rc;

use agg_gui::{
    theme::current_visuals, DrawCtx, Event, EventResult, HAnchor, Insets, Point, Rect, Size,
    VAnchor, Widget, WidgetBase,
};

use crate::mesh_raster::{IconImage, MAX_ICON_SIZE};

/// Side of the icon ghost in logical pixels — NodeDesigner's
/// `GHOST_SIZE_PX`.
pub const GHOST_ICON_SIDE: f64 = 48.0;

/// Opacity of the whole ghost, icon or pill (the ancestor's
/// `opacity: .85`).
const GHOST_ALPHA: f64 = 0.85;

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
    /// The item's rendered icon, when one was available at drag start.
    /// `None` keeps the glyph-and-label pill.
    icon: Option<IconImage>,
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
            icon: None,
            bounds: Rect::default(),
            base: WidgetBase::new()
                .with_h_anchor(HAnchor::LEFT)
                .with_v_anchor(VAnchor::BOTTOM),
            children: Vec::new(),
            close,
        }
    }

    /// Carry the item's rendered icon (the ancestor's `iconUrl`). `None`
    /// — nothing rendered for this payload — keeps the pill.
    pub fn with_icon(mut self, icon: Option<IconImage>) -> Self {
        self.icon = icon.filter(|i| i.width > 0 && i.height > 0);
        self
    }

    /// Is this ghost the icon image rather than the label fallback? The
    /// probe the tests (and the `icon` property) report.
    pub fn has_icon(&self) -> bool {
        self.icon.is_some()
    }

    /// Width the pill needs for its glyph + label.
    fn desired_width(&self) -> f64 {
        let text = crate::file_browser::widget_geom::measure(&self.label, TEXT_SIZE);
        PAD + TEXT_SIZE + PAD + text + PAD
    }

    /// The ghost's size: the ancestor's 48 × 48 square in icon mode, the
    /// measured pill otherwise.
    fn desired_size(&self) -> Size {
        if self.has_icon() {
            Size::new(GHOST_ICON_SIDE, GHOST_ICON_SIDE)
        } else {
            Size::new(self.desired_width(), GHOST_H)
        }
    }

    /// Rectangle for a cursor at `cursor` (window / world coords, Y-up),
    /// clamped so the ghost never leaves the window.
    ///
    /// Icon mode centres on the pointer, exactly like the ancestor's
    /// `left/top = client − 24`; the pill trails below and right so it
    /// does not cover what the cursor is about to hit.
    pub fn rect_for_cursor(&self, cursor: Point, window: Size) -> Rect {
        let Size {
            width: w,
            height: h,
        } = self.desired_size();
        let (x, y) = if self.has_icon() {
            (cursor.x - w * 0.5, cursor.y - h * 0.5)
        } else {
            // Y-up: "below and right of the cursor" is +x, -y.
            (cursor.x + CURSOR_OFFSET, cursor.y - CURSOR_OFFSET - h)
        };
        let x = if window.width > w {
            x.clamp(0.0, window.width - w)
        } else {
            0.0
        };
        let y = if window.height > h {
            y.clamp(0.0, window.height - h)
        } else {
            0.0
        };
        Rect::new(x, y, w, h)
    }
}

/// Edge length, in **device** pixels, to rasterize the ghost's icon at —
/// the shared device-scale rule applied to the ancestor's 48 px ghost.
pub fn icon_pixel_size() -> u32 {
    crate::mesh_raster::device_pixel_size(GHOST_ICON_SIDE)
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
        ctx.set_global_alpha(GHOST_ALPHA);
        // Icon mode: the item's own render, filling the square, with no
        // chrome behind it — the ancestor's bare `<img>` in a
        // transparent ghost div.
        //
        // A backend with no blit (agg-gui's `gl_renderer` implements
        // neither image entry point) is treated exactly like "no icon":
        // the full glyph-and-label pill, the same fallback the strip's
        // slots take — never a bare glyph floating with no chrome.
        if let Some(icon) = self.icon.as_ref().filter(|_| ctx.has_image_blit()) {
            ctx.draw_image_rgba_arc(&icon.rgba, icon.width, icon.height, 0.0, 0.0, w, h);
            ctx.set_global_alpha(1.0);
            return;
        }
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
        let text_x = PAD + TEXT_SIZE + PAD;
        ctx.fill_text(&self.glyph.to_string(), PAD, baseline);
        // The pill is normally measured to fit its label, but an icon
        // ghost that fell back to this look carries the ancestor's fixed
        // 48 px square instead — so the label is elided to whatever room
        // that leaves rather than spilling past the chrome.
        let label = crate::file_browser::widget_geom::elide(
            &self.label,
            (w - text_x - PAD).max(0.0),
            TEXT_SIZE,
        );
        ctx.fill_text(&label, text_x, baseline);
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
            // "is this the rendered icon, or the label fallback?" — the
            // harness's probe for step 6g-3.
            ("icon", self.has_icon().to_string()),
            ("closing", self.close.get().to_string()),
        ]
    }
}

/// Widget id the harness looks the ghost up by (design §6).
pub const GHOST_ID: &str = "drag-ghost";

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    fn ghost() -> DragGhost {
        DragGhost::new('X', "Box", Rc::new(Cell::new(false)))
    }

    /// A stand-in for a rendered node icon: any non-empty RGBA buffer.
    fn stub_icon(side: u32) -> IconImage {
        IconImage {
            width: side,
            height: side,
            rgba: Arc::new(vec![255u8; (side * side * 4) as usize]),
        }
    }

    fn icon_ghost() -> DragGhost {
        ghost().with_icon(Some(stub_icon(48)))
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

    /// With an icon the ghost is the ancestor's 48 × 48 square, centred
    /// on the pointer (`client − GHOST_SIZE / 2` on both axes).
    #[test]
    fn icon_ghost_is_a_48px_square_centred_on_the_cursor() {
        let g = icon_ghost();
        assert!(g.has_icon());
        let r = g.rect_for_cursor(Point::new(400.0, 300.0), Size::new(1280.0, 720.0));
        assert_eq!((r.width, r.height), (GHOST_ICON_SIDE, GHOST_ICON_SIDE));
        assert_eq!(r.x, 400.0 - GHOST_ICON_SIDE * 0.5);
        assert_eq!(r.y, 300.0 - GHOST_ICON_SIDE * 0.5);
    }

    /// The icon ghost clamps to the window like the pill does.
    #[test]
    fn icon_ghost_clamps_inside_the_window() {
        let g = icon_ghost();
        let window = Size::new(1280.0, 720.0);
        let r = g.rect_for_cursor(Point::new(1279.0, 2.0), window);
        assert!(r.x >= 0.0 && r.x + r.width <= window.width);
        assert!(r.y >= 0.0 && r.y + r.height <= window.height);
    }

    /// No icon (a file payload, or a type that has not rendered yet) →
    /// the glyph-and-label pill, and the property says so.
    #[test]
    fn a_ghost_without_an_icon_reports_the_fallback() {
        let plain = ghost();
        assert!(!plain.has_icon());
        assert!(plain.desired_width() > GHOST_ICON_SIDE * 0.5);
        let props = plain.properties();
        assert!(props.contains(&("icon", "false".to_string())), "{props:?}");
        let iconic = icon_ghost();
        assert!(iconic.properties().contains(&("icon", "true".to_string())));
        // A degenerate image is not an icon: it would blit as nothing.
        assert!(!ghost().with_icon(Some(stub_icon(0))).has_icon());
        assert!(!ghost().with_icon(None).has_icon());
    }
}
