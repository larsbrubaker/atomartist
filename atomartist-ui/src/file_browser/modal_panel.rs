//! `FileBrowserModal` — the Open/Save panel that lives inside the modal
//! sheet (design §4 `modal.rs` row, step 6c-1).
//!
//! This is only the *chrome*: a title, the shared [`FileBrowser`], and the
//! OK / Cancel pair. It owns no policy — where the pick comes from and how
//! it settles is [`super::modal`]'s business, handed here as two closures
//! and an "is OK meaningful right now" predicate. Splitting it that way
//! keeps this file pure assembly + geometry and keeps `modal.rs` free of
//! widget plumbing; neither approaches the 800-line cap.
//!
//! # Coordinates
//!
//! Panel-local and **Y-up**, like everything else in the browser: the
//! title strip is at the *top* (`y = h - TITLE_H`) and the button footer
//! sits at `y = 0`. [`ModalLayout`] is a free function over rectangles so
//! the UI tests can aim at the OK button without pixel-hunting.
//!
//! # The browser is placed after it is laid out
//!
//! [`FileBrowser::layout`] resets its own bounds to the origin (it
//! describes a size, not a placement), so `set_bounds` has to come *after*
//! the `layout` call — see the widget's module docs.

use std::rc::Rc;
use std::sync::Arc;

use agg_gui::text::Font;
use agg_gui::widgets::label::LabelAlign;
use agg_gui::{
    theme::current_visuals, Button, DrawCtx, Event, EventResult, HAnchor, Insets, Rect, Size,
    VAnchor, Widget, WidgetBase,
};

use super::widget::{BrowserMode, FileBrowser};
use crate::fa;

/// Height of the title strip across the top of the panel.
pub const TITLE_H: f64 = 34.0;
/// Height of the footer holding the OK / Cancel buttons.
pub const FOOTER_H: f64 = 46.0;
/// Padding around the panel's contents.
pub const PAD: f64 = 10.0;
/// One footer button.
pub const BUTTON_W: f64 = 104.0;
pub const BUTTON_H: f64 = 28.0;
/// Gap between the two footer buttons.
pub const BUTTON_GAP: f64 = 8.0;
/// Title font size.
pub const TITLE_SIZE: f64 = 15.0;

/// Where each piece of the panel lands, panel-local and Y-up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModalLayout {
    /// Strip holding the dialog title.
    pub title: Rect,
    /// The embedded [`FileBrowser`].
    pub browser: Rect,
    /// Confirm button ("Open" / "Save"), the rightmost of the pair.
    pub ok: Rect,
    /// Cancel button, immediately left of [`ModalLayout::ok`].
    pub cancel: Rect,
}

impl ModalLayout {
    /// Carve a panel of `available` size. Degenerate sizes yield
    /// zero-area rectangles rather than negative ones.
    pub fn compute(available: Size) -> ModalLayout {
        let w = available.width.max(0.0);
        let h = available.height.max(0.0);

        let title = Rect::new(0.0, (h - TITLE_H).max(0.0), w, TITLE_H.min(h));
        let browser_h = (h - TITLE_H - FOOTER_H).max(0.0);
        let browser = Rect::new(PAD, FOOTER_H, (w - 2.0 * PAD).max(0.0), browser_h);

        let button_y = ((FOOTER_H - BUTTON_H) * 0.5).max(0.0);
        let ok_x = (w - PAD - BUTTON_W).max(0.0);
        let ok = Rect::new(ok_x, button_y, BUTTON_W.min(w), BUTTON_H.min(h));
        let cancel = Rect::new(
            (ok_x - BUTTON_GAP - BUTTON_W).max(0.0),
            button_y,
            BUTTON_W.min(w),
            BUTTON_H.min(h),
        );

        ModalLayout {
            title,
            browser,
            ok,
            cancel,
        }
    }
}

/// The Open/Save panel. `children` are `[browser, ok, cancel]`.
pub struct FileBrowserModal {
    bounds: Rect,
    base: WidgetBase,
    children: Vec<Box<dyn Widget>>,
    mode: BrowserMode,
    title: String,
    layout: ModalLayout,
    /// Mirror of the OK button's gate, so `properties()` can report it to
    /// the inspector and the UI tests without poking at the button.
    ok_enabled: Rc<dyn Fn() -> bool>,
}

impl FileBrowserModal {
    /// Assemble the panel.
    ///
    /// `ok_enabled` gates the confirm button and is queried live (agg-gui
    /// re-asks it on every event and paint), so the button follows the
    /// selection / name field without anything having to rebuild it.
    pub fn new(
        mode: BrowserMode,
        font: Arc<Font>,
        browser: FileBrowser,
        ok_enabled: Rc<dyn Fn() -> bool>,
        on_ok: impl FnMut() + 'static,
        on_cancel: impl FnMut() + 'static,
    ) -> Self {
        let (title, ok_label, ok_glyph) = match mode {
            // `Embedded` never reaches the modal panel (it is the
            // favorites bar's face), but a panel built with it must still
            // be a working Open dialog rather than a panic.
            BrowserMode::Open | BrowserMode::Embedded => {
                ("Open Project", "Open", fa::FOLDER_OPEN)
            }
            BrowserMode::Save => ("Save Project", "Save", fa::SAVE),
        };
        let gate = Rc::clone(&ok_enabled);
        let ok_button = Button::new(ok_label, font.clone())
            .with_icon(ok_glyph, font.clone())
            .with_label_align(LabelAlign::Center)
            .with_enabled_fn(move || gate())
            .on_click(on_ok);
        let cancel_button = Button::new("Cancel", font.clone())
            .with_icon(fa::TIMES, font)
            .with_subtle()
            .with_label_align(LabelAlign::Center)
            .on_click(on_cancel);

        FileBrowserModal {
            bounds: Rect::default(),
            base: WidgetBase::new()
                .with_h_anchor(HAnchor::STRETCH)
                .with_v_anchor(VAnchor::STRETCH),
            children: vec![
                Box::new(browser),
                Box::new(ok_button),
                Box::new(cancel_button),
            ],
            mode,
            title: title.to_string(),
            layout: ModalLayout::compute(Size::new(0.0, 0.0)),
            ok_enabled,
        }
    }

    /// The regions as of the last layout — what the UI tests aim at.
    pub fn layout_rects(&self) -> ModalLayout {
        self.layout
    }

    pub fn mode(&self) -> BrowserMode {
        self.mode
    }
}

impl Widget for FileBrowserModal {
    fn type_name(&self) -> &'static str {
        "FileBrowserModal"
    }
    /// Stable id for the harness and the inspector (design §6).
    fn id(&self) -> Option<&str> {
        Some("file-browser-modal")
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
        self.bounds = Rect::new(0.0, 0.0, available.width, available.height);
        self.layout = ModalLayout::compute(available);
        let rects = self.layout;
        // Every child is laid out *then* placed: agg-gui widgets treat
        // `layout` as "how big are you" and reset their own origin.
        for (index, rect) in [(0usize, rects.browser), (1, rects.ok), (2, rects.cancel)] {
            if let Some(child) = self.children.get_mut(index) {
                child.layout(Size::new(rect.width, rect.height));
                child.set_bounds(rect);
            }
        }
        available
    }

    fn paint(&mut self, ctx: &mut dyn DrawCtx) {
        let visuals = current_visuals();
        let title = self.layout.title;
        if let Some(font) = agg_gui::font_settings::current_system_font() {
            ctx.set_font(font);
        }
        ctx.set_font_size(TITLE_SIZE);
        ctx.set_fill_color(visuals.text_color);
        // Y-up: the baseline sits a little above the strip's bottom edge.
        let baseline = title.y + (title.height - TITLE_SIZE) * 0.5 + 1.0;
        ctx.fill_text(&self.title, title.x + PAD + 4.0, baseline);

        ctx.set_stroke_color(visuals.separator);
        ctx.set_line_width(1.0);
        ctx.begin_path();
        ctx.move_to(title.x + PAD, title.y);
        ctx.line_to(title.x + title.width - PAD, title.y);
        ctx.stroke();
    }

    fn on_event(&mut self, _event: &Event) -> EventResult {
        // The panel itself is inert — the browser and the two buttons are
        // real children and get the events first. Anything that reaches
        // here falls through to the sheet, whose scrim swallows it.
        EventResult::Ignored
    }

    fn properties(&self) -> Vec<(&'static str, String)> {
        vec![
            ("mode", self.mode.as_str().to_string()),
            ("title", self.title.clone()),
            ("ok_enabled", (self.ok_enabled)().to_string()),
        ]
    }
}
