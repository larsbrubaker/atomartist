//! Circular icon button that opens a small popup menu of choices.
//!
//! Composes [`CircularIconButton`] (for the closed-state visual)
//! with a custom popup widget that pops downward from the button on
//! click. Selecting an entry writes its value into an
//! `Arc<Mutex<T>>` slot and closes the popup.
//!
//! ## Painting via `paint_global_overlay`
//!
//! MatterCAD's HUD dropdowns sit inside a narrow circular bay and
//! their popup menu needs to overflow the bay (and the sibling
//! buttons below it) while still painting in front of the rest of
//! the viewport. agg-gui provides the `paint_global_overlay` /
//! `hit_test_global_overlay` pair exactly for this case: the popup
//! is painted in a post-pass after the normal tree, so the parent's
//! `clip_children_rect` does NOT clip it, and event routing uses
//! `global_overlay_hit_path` so clicks anywhere inside `popup_rect`
//! reach the dropdown's `on_event` even though they are outside the
//! widget's own `bounds()`.
//!
//! This is deliberately a lightweight alternative to agg-gui's
//! `ComboBox` / `PopupMenu`: those widgets pull in modal focus +
//! caret-friendly styling that doesn't match MatterCAD's circular
//! HUD-button aesthetic. The dropdown here is just a list of small
//! pill-shaped buttons rendered below the trigger circle, dismissed
//! by clicking outside.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use agg_gui::{
    text::Font, theme::current_visuals, Color, DrawCtx, Event, EventResult, HAnchor, Insets,
    MouseButton, Point, Rect, Size, VAnchor, Widget, WidgetBase,
};

use crate::circular_icon_button::CircularIconButton;
use crate::icons::IconKind;
use crate::mattercad_icons::MatterCadIcon;

/// One row in the popup menu — text label plus the value to write
/// into the bound `Arc<Mutex<T>>` when the row is selected.
pub struct DropdownItem<T: Clone + Send + 'static> {
    pub label: String,
    pub value: T,
}

/// Circular icon-button + popup-menu composite.  See module docs.
pub struct CircularDropdown<T: Clone + Send + PartialEq + 'static> {
    bounds: Rect,
    base: WidgetBase,
    button: CircularIconButton,
    items: Rc<Vec<DropdownItem<T>>>,
    value: Arc<Mutex<T>>,
    open: Rc<RefCell<bool>>,
    font: Arc<Font>,
    children_storage: Vec<Box<dyn Widget>>,
    /// Geometry of the open popup in **widget-local** coords
    /// (origin at the dropdown's bottom-left). Recomputed on layout,
    /// read by `paint_global_overlay`, `on_event`, and
    /// `hit_test_global_overlay`.
    popup_rect: Rect,
    /// When set, this closure is called every paint to derive the
    /// MatterCAD PNG icon that should appear in the closed-state
    /// button from the current value. Used by the render-mode
    /// dropdown so the bubble reflects whether Shaded / Outlines /
    /// Polygons is active, matching MatterCAD's `ViewStyleButton`
    /// behaviour.
    value_to_icon: Option<Rc<dyn Fn(&T) -> Option<MatterCadIcon>>>,
    /// When set, this closure is called every paint to derive a
    /// short text label (e.g. "1", "5", "-") to render centred on
    /// the closed-state button instead of an icon. Mirrors
    /// MatterCAD's `GridOptionsPanel.textButton` which shows the
    /// snap distance value inside the trigger.
    value_to_label: Option<Rc<dyn Fn(&T) -> String>>,
}

impl<T: Clone + Send + PartialEq + 'static> CircularDropdown<T> {
    pub fn new(
        icon: IconKind,
        items: Vec<DropdownItem<T>>,
        value: Arc<Mutex<T>>,
        font: Arc<Font>,
    ) -> Self {
        Self::new_with_image(icon, None, items, value, font)
    }

    /// Build a dropdown whose closed-state icon is one of MatterCAD's
    /// bundled PNGs (rather than the hand-drawn `IconKind`).
    pub fn new_with_image(
        icon: IconKind,
        image_icon: Option<MatterCadIcon>,
        items: Vec<DropdownItem<T>>,
        value: Arc<Mutex<T>>,
        font: Arc<Font>,
    ) -> Self {
        let open = Rc::new(RefCell::new(false));
        let open_for_click = open.clone();
        let mut button = CircularIconButton::new(icon)
            .on_click(move || {
                let mut o = open_for_click.borrow_mut();
                *o = !*o;
            });
        if let Some(img) = image_icon {
            button = button.with_image_icon(img);
        }
        Self {
            bounds: Rect::default(),
            base: WidgetBase::new()
                .with_h_anchor(HAnchor::FIT)
                .with_v_anchor(VAnchor::FIT)
                .with_min_size(Size::new(20.0, 20.0))
                .with_max_size(Size::new(36.0, 36.0)),
            button,
            items: Rc::new(items),
            value,
            open,
            font,
            children_storage: Vec::new(),
            popup_rect: Rect::default(),
            value_to_icon: None,
            value_to_label: None,
        }
    }

    /// Map the current value to a MatterCAD PNG that should be shown
    /// inside the closed-state button. Returning `None` keeps the
    /// initially-configured icon.
    pub fn with_value_to_icon<F>(mut self, f: F) -> Self
    where
        F: Fn(&T) -> Option<MatterCadIcon> + 'static,
    {
        self.value_to_icon = Some(Rc::new(f));
        self
    }

    /// Map the current value to a short string to render centred on
    /// the closed-state button. Setting this takes priority over the
    /// icon (the bubble becomes text instead of a glyph).
    pub fn with_value_to_label<F>(mut self, f: F) -> Self
    where
        F: Fn(&T) -> String + 'static,
    {
        self.value_to_label = Some(Rc::new(f));
        self
    }

    pub fn current_value(&self) -> T {
        self.value.lock().unwrap().clone()
    }

    fn is_open(&self) -> bool {
        *self.open.borrow()
    }

    fn close(&self) {
        *self.open.borrow_mut() = false;
    }

    /// Compute the popup geometry in **widget-local** coordinates.
    /// The popup opens DOWNWARD from the button — in agg-gui Y-up
    /// coords that means at lower Y values.
    fn compute_popup_rect(&self) -> Rect {
        let row_h = 22.0;
        let pad = 4.0;
        let width = 110.0;
        let n = self.items.len() as f64;
        let h = n * row_h + pad * 2.0;
        // Popup centred horizontally on the button; its top edge
        // sits just below the button's bottom edge. With
        // widget-local coords the bottom edge is y=0, so the
        // popup's top is at -4 and its bottom at -4-h.
        let cx_local = self.bounds.width * 0.5;
        let bx = cx_local - width * 0.5;
        let by_top = -4.0;
        let by = by_top - h;
        Rect::new(bx, by, width, h)
    }

    fn row_rect_in_popup(&self, idx: usize) -> Rect {
        let row_h = 22.0;
        let pad = 4.0;
        // Top row visually = first item; in Y-up, top of popup is at
        // popup_rect.y + popup_rect.height.
        let top_y = self.popup_rect.y + self.popup_rect.height - pad;
        let y = top_y - row_h * (idx as f64 + 1.0);
        Rect::new(
            self.popup_rect.x + pad,
            y,
            self.popup_rect.width - pad * 2.0,
            row_h,
        )
    }

    /// Push the current dropdown value into the closed-state
    /// button's icon / text label, applying the (optional) closures
    /// configured by `with_value_to_icon` / `with_value_to_label`.
    /// Called from `paint` (and `paint_global_overlay`) so the
    /// bubble always reflects the live value.
    fn sync_button_to_value(&mut self) {
        let value = self.current_value();
        if let Some(ref f) = self.value_to_icon.clone() {
            self.button.set_image_icon(f(&value));
        }
        if let Some(ref f) = self.value_to_label.clone() {
            self.button.set_text_label(Some(f(&value)));
        }
    }

    /// Paint the popup background and item rows at the current
    /// `popup_rect`. Shared between `paint_global_overlay` (the
    /// production path) and any direct debugging callers — the rest
    /// of the dropdown's paint relies on agg-gui's global-overlay
    /// pass scheduling this code at the right Z-level.
    fn paint_popup(&self, ctx: &mut dyn DrawCtx) {
        let v = current_visuals();
        let bg = v.widget_bg;
        let stroke = v.widget_stroke;
        let p = self.popup_rect;
        ctx.set_fill_color(bg);
        ctx.begin_path();
        ctx.rect(p.x, p.y, p.width, p.height);
        ctx.fill();
        ctx.set_stroke_color(stroke);
        ctx.set_line_width(1.0);
        ctx.begin_path();
        ctx.rect(p.x + 0.5, p.y + 0.5, p.width - 1.0, p.height - 1.0);
        ctx.stroke();

        // Rows.
        let current = self.current_value();
        if let Some(font) = agg_gui::font_settings::current_system_font().or(Some(self.font.clone()))
        {
            ctx.set_font(font);
        }
        ctx.set_font_size(12.0);

        for (i, item) in self.items.iter().enumerate() {
            let r = self.row_rect_in_popup(i);
            let active = item.value == current;
            let row_bg = if active { v.accent } else { v.widget_bg };
            ctx.set_fill_color(row_bg);
            ctx.begin_path();
            ctx.rect(r.x, r.y, r.width, r.height);
            ctx.fill();
            ctx.set_fill_color(if active { Color::white() } else { v.text_color });
            // Approximate label centring — y baseline near vertical
            // centre of the row.
            ctx.fill_text(&item.label, r.x + 8.0, r.y + r.height * 0.3);
        }
    }
}

impl<T: Clone + Send + PartialEq + 'static> Widget for CircularDropdown<T> {
    fn type_name(&self) -> &'static str { "CircularDropdown" }
    fn bounds(&self) -> Rect { self.bounds }
    fn set_bounds(&mut self, b: Rect) {
        self.bounds = b;
        // The button fills our bounds. Keep its bounds in
        // widget-local coords so its inner paint draws into the same
        // (0, 0)-rooted canvas the dropdown does.
        self.button.set_bounds(Rect::new(0.0, 0.0, b.width, b.height));
    }
    fn children(&self) -> &[Box<dyn Widget>] { &[] }
    fn children_mut(&mut self) -> &mut Vec<Box<dyn Widget>> {
        &mut self.children_storage
    }
    fn h_anchor(&self) -> HAnchor { self.base.h_anchor }
    fn v_anchor(&self) -> VAnchor { self.base.v_anchor }
    fn min_size(&self) -> Size { self.base.min_size }
    fn max_size(&self) -> Size { self.base.max_size }
    fn widget_base(&self) -> Option<&WidgetBase> { Some(&self.base) }

    fn layout(&mut self, available: Size) -> Size {
        let s = self.button.layout(available);
        self.bounds = Rect::new(0.0, 0.0, s.width, s.height);
        self.popup_rect = self.compute_popup_rect();
        s
    }

    fn paint(&mut self, ctx: &mut dyn DrawCtx) {
        // Reflect the current bound value in the button bubble
        // before painting it — the icon / text on the trigger has
        // to stay in sync with the active option even when the
        // user changes the value externally.
        self.sync_button_to_value();
        self.button.paint(ctx);
    }

    /// Paint the popup AFTER the rest of the tree has finished. The
    /// app driver walks every widget's `paint_global_overlay` in
    /// paint order; popup contents drawn here are not subject to the
    /// parent's `clip_children_rect`, so a popup that overflows the
    /// trigger button's 32x32 bay still renders correctly.
    fn paint_global_overlay(&mut self, ctx: &mut dyn DrawCtx) {
        if !self.is_open() {
            return;
        }
        self.paint_popup(ctx);
    }

    /// Tell `global_overlay_hit_path` whether a click at `local_pos`
    /// should be routed to this dropdown. Only true while the popup
    /// is open AND the cursor sits over the popup rect (or anywhere
    /// inside the trigger button so dismissing by clicking the
    /// trigger again still works).
    fn hit_test_global_overlay(&self, local_pos: Point) -> bool {
        if !self.is_open() {
            return false;
        }
        rect_contains(self.popup_rect, local_pos)
    }

    fn on_event(&mut self, event: &Event) -> EventResult {
        // Forward to the button first — closed-state click toggles
        // open (and clicks INSIDE the trigger when the popup is
        // already open should toggle it back closed via the same
        // button handler).
        let result = self.button.on_event(event);
        if result == EventResult::Consumed {
            return EventResult::Consumed;
        }
        if !self.is_open() {
            return result;
        }
        // Open: handle popup row clicks. Events arrive in
        // widget-local coords courtesy of `dispatch_event` (which
        // followed the global-overlay path that
        // `hit_test_global_overlay` built up).
        match event {
            Event::MouseDown { pos, button, .. } if *button == MouseButton::Left => {
                let p = *pos;
                if !rect_contains(self.popup_rect, p) {
                    self.close();
                    return EventResult::Consumed;
                }
                for (i, item) in self.items.iter().enumerate() {
                    let rr = self.row_rect_in_popup(i);
                    if rect_contains(rr, p) {
                        *self.value.lock().unwrap() = item.value.clone();
                        self.close();
                        return EventResult::Consumed;
                    }
                }
                EventResult::Consumed
            }
            // MouseUp inside the popup keeps the dropdown open even
            // though the button widget would normally treat any
            // MouseUp as "click finished"; consume it so the
            // ViewportOverlay's fallback dispatch doesn't claim the
            // popup region for something else.
            Event::MouseUp { pos, button, .. }
                if *button == MouseButton::Left && rect_contains(self.popup_rect, *pos) =>
            {
                EventResult::Consumed
            }
            _ => result,
        }
    }
}

fn rect_contains(r: Rect, p: Point) -> bool {
    p.x >= r.x && p.x < r.x + r.width && p.y >= r.y && p.y < r.y + r.height
}

#[allow(dead_code)]
fn _silence_unused() {
    let _ = Insets::ZERO;
}

#[cfg(test)]
mod tests {
    use super::*;

    const FONT_BYTES: &[u8] = include_bytes!(
        "../../../agg-gui/agg-gui/assets/fonts/NotoSans-Regular.ttf"
    );

    fn dummy_font() -> Arc<Font> {
        agg_gui::font_settings::current_system_font().unwrap_or_else(|| {
            Arc::new(Font::from_bytes(FONT_BYTES.to_vec()).expect("bundled NotoSans"))
        })
    }

    #[test]
    fn dropdown_starts_closed() {
        let v = Arc::new(Mutex::new(0i32));
        let d = CircularDropdown::new(
            IconKind::Snap,
            vec![DropdownItem { label: "A".into(), value: 1 }],
            v.clone(),
            dummy_font(),
        );
        assert!(!d.is_open());
    }

    #[test]
    fn clicking_button_toggles_open() {
        let v = Arc::new(Mutex::new(0i32));
        let mut d = CircularDropdown::new(
            IconKind::Snap,
            vec![
                DropdownItem { label: "A".into(), value: 1 },
                DropdownItem { label: "B".into(), value: 2 },
            ],
            v.clone(),
            dummy_font(),
        );
        d.layout(Size::new(30.0, 30.0));
        let p = Point::new(15.0, 15.0);
        d.on_event(&Event::MouseDown {
            pos: p,
            button: MouseButton::Left,
            modifiers: agg_gui::Modifiers::default(),
        });
        d.on_event(&Event::MouseUp {
            pos: p,
            button: MouseButton::Left,
            modifiers: agg_gui::Modifiers::default(),
        });
        assert!(d.is_open(), "click on the button should open the dropdown");
    }

    #[test]
    fn clicking_row_writes_value_and_closes() {
        let v = Arc::new(Mutex::new(0i32));
        let mut d = CircularDropdown::new(
            IconKind::Snap,
            vec![
                DropdownItem { label: "A".into(), value: 10 },
                DropdownItem { label: "B".into(), value: 20 },
            ],
            v.clone(),
            dummy_font(),
        );
        d.layout(Size::new(30.0, 30.0));
        // Open first via button click.
        let bp = Point::new(15.0, 15.0);
        d.on_event(&Event::MouseDown {
            pos: bp,
            button: MouseButton::Left,
            modifiers: agg_gui::Modifiers::default(),
        });
        d.on_event(&Event::MouseUp {
            pos: bp,
            button: MouseButton::Left,
            modifiers: agg_gui::Modifiers::default(),
        });
        assert!(d.is_open());
        // Now click somewhere inside the second row. The popup is
        // anchored below the button in widget-local coords (negative
        // Y), so we resolve the row rect explicitly.
        let row1 = d.row_rect_in_popup(1);
        let rp = Point::new(row1.x + row1.width * 0.5, row1.y + row1.height * 0.5);
        d.on_event(&Event::MouseDown {
            pos: rp,
            button: MouseButton::Left,
            modifiers: agg_gui::Modifiers::default(),
        });
        assert!(!d.is_open(), "selecting a row should close the popup");
        assert_eq!(*v.lock().unwrap(), 20);
    }

    #[test]
    fn hit_test_global_overlay_only_true_when_open_and_in_popup() {
        let v = Arc::new(Mutex::new(0i32));
        let mut d = CircularDropdown::new(
            IconKind::Snap,
            vec![
                DropdownItem { label: "A".into(), value: 10 },
                DropdownItem { label: "B".into(), value: 20 },
            ],
            v.clone(),
            dummy_font(),
        );
        d.layout(Size::new(30.0, 30.0));
        // Closed → never claims the global overlay, even inside the
        // theoretical popup rect.
        let row1 = d.row_rect_in_popup(1);
        let inside = Point::new(row1.x + row1.width * 0.5, row1.y + row1.height * 0.5);
        assert!(!d.hit_test_global_overlay(inside));

        // Open the dropdown via a button click.
        let bp = Point::new(15.0, 15.0);
        d.on_event(&Event::MouseDown {
            pos: bp,
            button: MouseButton::Left,
            modifiers: agg_gui::Modifiers::default(),
        });
        d.on_event(&Event::MouseUp {
            pos: bp,
            button: MouseButton::Left,
            modifiers: agg_gui::Modifiers::default(),
        });
        assert!(d.is_open());
        assert!(d.hit_test_global_overlay(inside));
        // Points outside the popup don't claim the overlay either.
        assert!(!d.hit_test_global_overlay(Point::new(-1000.0, -1000.0)));
    }

    #[test]
    fn value_to_label_drives_button_text() {
        let v = Arc::new(Mutex::new(7i32));
        let mut d = CircularDropdown::new(
            IconKind::Snap,
            vec![DropdownItem { label: "7".into(), value: 7 }],
            v.clone(),
            dummy_font(),
        )
        .with_value_to_label(|n: &i32| n.to_string());
        d.layout(Size::new(30.0, 30.0));
        // sync_button_to_value runs on `paint`; force it here.
        d.sync_button_to_value();
        // No assertion on rendered glyphs, but the closure must run
        // without panicking on the bound value (smoke test).
        assert_eq!(d.current_value(), 7);
    }
}
