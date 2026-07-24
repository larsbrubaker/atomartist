//! Drill-in navigation chrome — a back button + breadcrumb trail that
//! appears in the top row while the user has drilled into a component.
//!
//! The bar is a custom `Widget` (like [`crate::status_bar::StatusBar`])
//! that reads [`AppState::edit_stack`] on every layout/paint so its
//! labels never go stale as the stack changes (double-click drill-in,
//! back button, breadcrumb clicks, File → New). It renders, left to
//! right: a chevron-left back button, then breadcrumb segments —
//! "Top Level" followed by each drilled-in level's label — joined by
//! chevron separators. Ancestor segments render link-styled and are
//! clickable; the last (current) segment is plain body text and inert.
//!
//! When [`AppState::edit_depth`] is `0` the whole bar hides itself via
//! [`Widget::is_visible`], so the enclosing `FlexRow` gives it no slot
//! and it consumes no pointer events.
//!
//! Navigation wiring (see [`crate::app_state_drill`]):
//!   - back button  → [`AppState::exit_one`]
//!   - "Top Level"  → [`AppState::exit_to`]`(0)`
//!   - ancestor `i` → [`AppState::exit_to`]`(i)`

use agg_gui::{
    font_settings, text::measure_text_metrics, DrawCtx, Event, EventResult, HAnchor,
    Insets, MouseButton, Rect, Size, VAnchor, Widget, WidgetBase,
};
use agg_gui::widgets::menu::MENU_BAR_H;

use crate::app_state::AppState;

/// Font Awesome chevron code points (same face `top_menu_bar.rs` renders
/// through the system font's fallback chain). Sourced from [`crate::fa`].
const ICON_CHEVRON_LEFT: char = crate::fa::CHEVRON_LEFT;
const ICON_CHEVRON_RIGHT: char = crate::fa::CHEVRON_RIGHT;

/// Left/right padding at the ends of the bar.
pub const PAD_X: f64 = 8.0;
/// Fixed hit width of the back button. Fixed (not glyph-measured) so the
/// clickable region is deterministic even when the icon glyph is missing
/// from the active font (e.g. the plain-NotoSans test harness).
pub const BACK_BTN_W: f64 = 24.0;
/// Gap between the back button and the first crumb.
pub const GAP: f64 = 6.0;
/// Horizontal span reserved for a chevron separator between two crumbs.
pub const SEP_W: f64 = 16.0;
/// Body/label font size.
const FONT_SIZE: f64 = 12.0;
/// Bar height — matches the menu bar so it aligns in the top row.
const BAR_H: f64 = MENU_BAR_H;

/// Local x of the back button's centre — a test convenience so callers
/// can synthesise a click that reliably lands on the back affordance.
pub const BACK_BUTTON_CENTER_X: f64 = PAD_X + BACK_BTN_W * 0.5;
/// Local x a few pixels into the first ("Top Level") crumb — reliably
/// inside its text box regardless of the active font's advances.
pub const FIRST_CRUMB_HIT_X: f64 = PAD_X + BACK_BTN_W + GAP + 4.0;

/// One laid-out breadcrumb segment and its click target.
#[derive(Clone)]
struct Crumb {
    text: String,
    x0: f64,
    x1: f64,
    /// `Some(depth)` → clickable ancestor that calls `exit_to(depth)`;
    /// `None` → the current (top-of-stack) location, non-interactive.
    target_depth: Option<usize>,
}

pub struct BreadcrumbBar {
    bounds: Rect,
    children: Vec<Box<dyn Widget>>,
    base: WidgetBase,
    state: AppState,
    /// Segments computed during `layout`, consumed by `paint` +
    /// `on_event`. Empty when at the root (bar hidden).
    crumbs: Vec<Crumb>,
    /// Total content width computed during `layout`.
    content_w: f64,
}

impl BreadcrumbBar {
    pub fn new(state: AppState) -> Self {
        Self {
            bounds: Rect::default(),
            children: Vec::new(),
            base: WidgetBase::new()
                .with_h_anchor(HAnchor::FIT)
                .with_v_anchor(VAnchor::CENTER)
                .with_max_size(Size::new(f64::INFINITY, BAR_H)),
            state,
            crumbs: Vec::new(),
            content_w: 0.0,
        }
    }

    /// Snapshot the breadcrumb labels from the edit stack under a short
    /// lock. Returns an empty vec at the root (depth 0). The lock is
    /// released before any other `AppState` call.
    fn segment_labels(&self) -> Vec<String> {
        let stack = self.state.edit_stack.lock().unwrap();
        if stack.is_empty() {
            return Vec::new();
        }
        let mut labels = Vec::with_capacity(stack.len() + 1);
        labels.push("Top Level".to_string());
        for level in stack.iter() {
            labels.push(level.label.clone());
        }
        labels
    }

    /// Measure a label's advance in the current system font. Falls back
    /// to a rough estimate when no font is installed (should not happen
    /// once `build_app` has run, but keeps layout total-order stable).
    fn measure_label(text: &str) -> f64 {
        match font_settings::current_system_font() {
            Some(font) => measure_text_metrics(&font, text, FONT_SIZE).width,
            None => text.chars().count() as f64 * FONT_SIZE * 0.55,
        }
    }

    /// Recompute `crumbs` + `content_w` from the live edit stack.
    fn rebuild(&mut self) {
        let labels = self.segment_labels();
        self.crumbs.clear();
        if labels.is_empty() {
            self.content_w = 0.0;
            return;
        }
        let last = labels.len() - 1;
        let mut x = PAD_X + BACK_BTN_W + GAP;
        for (i, label) in labels.iter().enumerate() {
            let w = Self::measure_label(label);
            let target_depth = if i < last { Some(i) } else { None };
            self.crumbs.push(Crumb {
                text: label.clone(),
                x0: x,
                x1: x + w,
                target_depth,
            });
            x += w;
            if i < last {
                x += SEP_W;
            }
        }
        self.content_w = x + PAD_X;
    }

    /// Hit-test the stored crumb regions against a local x, returning the
    /// navigation action to perform (if any).
    fn action_at(&self, local_x: f64) -> Option<BreadcrumbAction> {
        if self.crumbs.is_empty() {
            return None;
        }
        if local_x >= PAD_X && local_x < PAD_X + BACK_BTN_W {
            return Some(BreadcrumbAction::Back);
        }
        for crumb in &self.crumbs {
            if local_x >= crumb.x0 && local_x < crumb.x1 {
                return crumb.target_depth.map(BreadcrumbAction::ExitTo);
            }
        }
        None
    }
}

enum BreadcrumbAction {
    Back,
    ExitTo(usize),
}

impl Widget for BreadcrumbBar {
    fn type_name(&self) -> &'static str {
        "BreadcrumbBar"
    }
    /// Stable id for the test harness / inspector lookup.
    fn id(&self) -> Option<&str> {
        Some("breadcrumb-bar")
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
    fn h_anchor(&self) -> HAnchor {
        self.base.h_anchor
    }
    fn v_anchor(&self) -> VAnchor {
        self.base.v_anchor
    }
    fn min_size(&self) -> Size {
        self.base.min_size
    }
    fn max_size(&self) -> Size {
        self.base.max_size
    }
    fn margin(&self) -> Insets {
        self.base.margin
    }
    fn widget_base(&self) -> Option<&WidgetBase> {
        Some(&self.base)
    }

    /// Hidden at the root so the enclosing `FlexRow` allocates no slot
    /// and pointer events pass straight through.
    fn is_visible(&self) -> bool {
        self.state.edit_depth() > 0
    }

    fn layout(&mut self, _available: Size) -> Size {
        self.rebuild();
        let w = self.content_w;
        self.bounds = Rect::new(0.0, 0.0, w, BAR_H);
        Size::new(w, BAR_H)
    }

    fn paint(&mut self, ctx: &mut dyn DrawCtx) {
        if self.crumbs.is_empty() {
            return;
        }
        let h = self.bounds.height;
        let w = self.bounds.width;
        if w <= 0.0 || h <= 0.0 {
            return;
        }

        ctx.save();
        ctx.clip_rect(0.0, 0.0, w, h);

        if let Some(font) = font_settings::current_system_font() {
            ctx.set_font(font);
        }
        ctx.set_font_size(FONT_SIZE);

        let visuals = ctx.visuals();
        // Y-up local space (origin bottom-left): centre the baseline
        // using the font's ascent/descent, matching StatusBar's approach.
        let metrics = ctx
            .measure_text("Ag")
            .map(|m| (m.ascent, m.descent))
            .unwrap_or((FONT_SIZE * 0.8, -FONT_SIZE * 0.2));
        let baseline_y = (h - metrics.0 - metrics.1) * 0.5;

        // Back button — chevron-left, centred in its fixed region.
        let back_glyph = ICON_CHEVRON_LEFT.to_string();
        let back_gw = ctx.measure_text(&back_glyph).map(|m| m.width).unwrap_or(0.0);
        ctx.set_fill_color(visuals.text_color);
        ctx.fill_text(
            &back_glyph,
            PAD_X + (BACK_BTN_W - back_gw) * 0.5,
            baseline_y,
        );

        // Crumbs + separators.
        let last = self.crumbs.len() - 1;
        let sep_glyph = ICON_CHEVRON_RIGHT.to_string();
        let sep_gw = ctx.measure_text(&sep_glyph).map(|m| m.width).unwrap_or(0.0);
        for (i, crumb) in self.crumbs.iter().enumerate() {
            // Ancestors render link-styled; the current location is body
            // text (visually distinct + non-clickable).
            let color = if crumb.target_depth.is_some() {
                visuals.text_link
            } else {
                visuals.text_color
            };
            ctx.set_fill_color(color);
            ctx.fill_text(&crumb.text, crumb.x0, baseline_y);

            if i < last {
                let next_x0 = self.crumbs[i + 1].x0;
                let mid = (crumb.x1 + next_x0) * 0.5;
                ctx.set_fill_color(visuals.text_dim);
                ctx.fill_text(&sep_glyph, mid - sep_gw * 0.5, baseline_y);
            }
        }

        ctx.restore();
    }

    fn on_event(&mut self, event: &Event) -> EventResult {
        if let Event::MouseDown {
            pos,
            button: MouseButton::Left,
            ..
        } = event
        {
            if let Some(action) = self.action_at(pos.x) {
                match action {
                    BreadcrumbAction::Back => self.state.exit_one(),
                    BreadcrumbAction::ExitTo(depth) => self.state.exit_to(depth),
                }
                return EventResult::Consumed;
            }
        }
        EventResult::Ignored
    }

    /// Surface the trail + depth for the inspector and UI tests (read via
    /// the harness's `snapshot()` → `InspectorNode::properties`).
    fn properties(&self) -> Vec<(&'static str, String)> {
        let trail: Vec<&str> = self.crumbs.iter().map(|c| c.text.as_str()).collect();
        vec![
            ("depth", self.state.edit_depth().to_string()),
            ("trail", trail.join(" > ")),
        ]
    }
}
