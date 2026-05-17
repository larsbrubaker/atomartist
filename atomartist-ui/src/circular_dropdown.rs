//! Circular icon button that opens a small popup menu of choices.
//!
//! Composes [`CircularIconButton`] (for the closed-state visual)
//! with a custom popup widget that pops downward from the button on
//! click. Selecting an entry writes its value into an
//! `Arc<Mutex<T>>` slot and closes the popup.
//!
//! This is a deliberately lightweight alternative to agg-gui's
//! `ComboBox` / `PopupMenu`: those widgets pull in modal focus +
//! caret-friendly styling which doesn't match MatterCAD's circular
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
    /// Geometry of the open popup — recomputed on layout, read by
    /// `paint` and `on_event` to know where the menu rows sit. Y-up
    /// coords (origin at the widget's bottom-left).
    popup_rect: Rect,
}

impl<T: Clone + Send + PartialEq + 'static> CircularDropdown<T> {
    pub fn new(
        icon: IconKind,
        items: Vec<DropdownItem<T>>,
        value: Arc<Mutex<T>>,
        font: Arc<Font>,
    ) -> Self {
        let open = Rc::new(RefCell::new(false));
        let open_for_click = open.clone();
        let button = CircularIconButton::new(icon)
            .with_overlay(IconKind::DropdownChevron)
            .on_click(move || {
                let mut o = open_for_click.borrow_mut();
                *o = !*o;
            });
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
        }
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

    /// Compute the popup geometry given the button's current bounds.
    /// Popup opens DOWNWARD from the button — in agg-gui Y-up coords
    /// that means at lower Y values.
    fn compute_popup_rect(&self) -> Rect {
        let row_h = 22.0;
        let pad = 4.0;
        let width = 110.0;
        let n = self.items.len() as f64;
        let h = n * row_h + pad * 2.0;
        // Popup centred horizontally on the button; its top edge sits
        // just below the button.
        let bx = self.bounds.x + self.bounds.width * 0.5 - width * 0.5;
        let by_top = self.bounds.y - 4.0; // a small gap below
        let by = by_top - h;
        Rect::new(bx, by, width, h)
    }

    fn row_rect_in_popup(&self, idx: usize) -> Rect {
        let row_h = 22.0;
        let pad = 4.0;
        let n = self.items.len();
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
            .with_clamp_n(n, idx)
    }
}

/// Tiny helper trait to keep the `row_rect_in_popup` call chain
/// readable.  Doesn't actually modify the rect — just passes through.
trait RectExt {
    fn with_clamp_n(self, _n: usize, _idx: usize) -> Self;
}
impl RectExt for Rect {
    fn with_clamp_n(self, _n: usize, _idx: usize) -> Self { self }
}

impl<T: Clone + Send + PartialEq + 'static> Widget for CircularDropdown<T> {
    fn type_name(&self) -> &'static str { "CircularDropdown" }
    fn bounds(&self) -> Rect { self.bounds }
    fn set_bounds(&mut self, b: Rect) {
        self.bounds = b;
        // The button fills our bounds.
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
        // Paint the closed-state button first.
        self.button.paint(ctx);

        if !self.is_open() {
            return;
        }

        // Popup background.
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
        if let Some(font) = agg_gui::font_settings::current_system_font().or(Some(self.font.clone())) {
            ctx.set_font(font);
        }
        ctx.set_font_size(12.0);

        for (i, item) in self.items.iter().enumerate() {
            let r = self.row_rect_in_popup(i);
            let active = item.value == current;
            let bg = if active { v.accent } else { v.widget_bg };
            ctx.set_fill_color(bg);
            ctx.begin_path();
            ctx.rect(r.x, r.y, r.width, r.height);
            ctx.fill();
            ctx.set_fill_color(if active { Color::white() } else { v.text_color });
            // Approximate label centring — y baseline near vertical
            // centre of the row.
            ctx.fill_text(&item.label, r.x + 8.0, r.y + r.height * 0.3);
        }
        // Unused field reminder.
        let _ = &self.font;
    }

    fn on_event(&mut self, event: &Event) -> EventResult {
        // Forward to the button first — closed-state click toggles open.
        let result = self.button.on_event(event);
        if result == EventResult::Consumed {
            return EventResult::Consumed;
        }
        if !self.is_open() {
            return result;
        }
        // Open: handle popup row clicks.
        match event {
            Event::MouseDown { pos, button, .. } if *button == MouseButton::Left => {
                // If outside the popup, close (and treat as "didn't
                // pick anything").  If inside a row rect, pick it.
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
            _ => result,
        }
    }
}

fn rect_contains(r: Rect, p: Point) -> bool {
    p.x >= r.x && p.x < r.x + r.width && p.y >= r.y && p.y < r.y + r.height
}

// Silence unused-import warning during incremental development.
#[allow(dead_code)]
fn _silence_unused() {
    let _ = Insets::ZERO;
}

#[cfg(test)]
mod tests {
    use super::*;

    // The bundled NotoSans font from agg-gui — same bytes the demo
    // shells load. Tests need a real font so `CircularDropdown::new`
    // can store it; we never paint text in these tests so the choice
    // of font is irrelevant.
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
        // Now click somewhere inside the second row.
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
}
