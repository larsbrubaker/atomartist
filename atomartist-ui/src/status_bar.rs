//! Bottom status bar — zoom %, version, node count, storage activity.
//!
//! A custom Widget that paints a thin horizontal strip with text labels
//! pulled from `AppState` each frame. Doesn't use a real Label widget
//! because the values change every frame and rebuilding Labels each
//! paint is wasteful. Width and height are STRETCH × natural baseline
//! so it sits flush at the bottom of the column.
//!
//! The middle of the bar is the storage segment — the UI surface for
//! [`crate::storage_ops`]:
//!
//! - While operations are in flight it shows
//!   [`AppState::storage_activity_text`] followed by a Font Awesome ✕;
//!   clicking that region calls [`AppState::cancel_pending_ops`].
//! - After them it shows [`AppState::last_notice`] — errors in red, info
//!   in normal text — until a newer notice replaces it or the user clicks
//!   to dismiss. Deliberately minimal; the toast / dialog treatment lands
//!   with the file-browser phase.
//! - When neither applies the segment vanishes and reserves no space.
//!
//! [`Widget::needs_draw`] reports `true` while an operation is pending, so
//! retained ancestors keep re-rastering this strip as progress advances.
//! That is what lets the pump's keep-alive skip the invalidation-epoch
//! bump (see the `storage_ops` module docs).

use agg_gui::{
    font_settings, text::measure_text_metrics, Color, DrawCtx, Event, EventResult, HAnchor,
    Insets, MouseButton, Point, Rect, Size, VAnchor, Widget, WidgetBase,
};

use crate::app_state::AppState;
use crate::storage_ops::{Notice, NoticeLevel};

const BAR_HEIGHT: f64 = 24.0;
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const FONT_SIZE: f64 = 11.0;

/// Left edge of the storage segment — clear of the zoom / version labels.
pub const STORAGE_X: f64 = 150.0;
/// Fixed hit width of the cancel affordance. Fixed rather than
/// glyph-measured so the clickable region is deterministic even when the
/// icon glyph is missing from the active font (the test harness runs
/// plain NotoSans).
pub const CANCEL_W: f64 = 16.0;
/// Gap between the activity text, the cancel button, and the notice.
const GAP: f64 = 6.0;

/// Error-notice colour. agg-gui's `Visuals` has no error token yet;
/// introducing one touches every palette, so it stays local until a
/// second call site justifies the upstream addition.
const ERROR_COLOR: Color = Color::rgb(0.85, 0.25, 0.25);

pub struct StatusBar {
    bounds: Rect,
    children: Vec<Box<dyn Widget>>,
    base: WidgetBase,
    state: AppState,
    /// Storage-segment content + hit regions, recomputed each `layout`
    /// and consumed by `paint` / `hit_test` / `on_event`.
    activity: Option<String>,
    notice: Option<Notice>,
    /// `[x0, x1)` of the cancel affordance, when an operation is pending.
    cancel_span: Option<(f64, f64)>,
    /// `[x0, x1)` of the notice text, when one is on display.
    notice_span: Option<(f64, f64)>,
}

impl StatusBar {
    pub fn new(state: AppState) -> Self {
        Self {
            bounds: Rect::default(),
            children: Vec::new(),
            base: WidgetBase::new()
                .with_h_anchor(HAnchor::STRETCH)
                .with_v_anchor(VAnchor::FIT)
                .with_max_size(Size::new(f64::INFINITY, BAR_HEIGHT)),
            state,
            activity: None,
            notice: None,
            cancel_span: None,
            notice_span: None,
        }
    }

    /// Measure a label's advance in the current system font. Falls back to
    /// a rough estimate when no font is installed, so layout stays stable.
    fn measure(text: &str) -> f64 {
        match font_settings::current_system_font() {
            Some(font) => measure_text_metrics(&font, text, FONT_SIZE).width,
            None => text.chars().count() as f64 * FONT_SIZE * 0.55,
        }
    }

    /// Pull the storage segment's content from `AppState` and lay out its
    /// hit regions left to right.
    fn rebuild(&mut self) {
        self.activity = self.state.storage_activity_text();
        self.notice = self.state.last_notice();
        self.cancel_span = None;
        self.notice_span = None;

        let mut x = STORAGE_X;
        if let Some(text) = &self.activity {
            x += Self::measure(text) + GAP;
            self.cancel_span = Some((x, x + CANCEL_W));
            x += CANCEL_W + GAP * 2.0;
        }
        if let Some(notice) = &self.notice {
            self.notice_span = Some((x, x + Self::measure(&notice.text)));
        }
    }

    /// Centre of the cancel affordance in widget-local x — what a test (or
    /// a future tooltip anchor) needs to aim at.
    pub fn cancel_center_x(&self) -> Option<f64> {
        self.cancel_span.map(|(x0, x1)| (x0 + x1) * 0.5)
    }

    fn span_contains(span: Option<(f64, f64)>, x: f64) -> bool {
        span.is_some_and(|(x0, x1)| x >= x0 && x < x1)
    }
}

impl Widget for StatusBar {
    fn type_name(&self) -> &'static str { "StatusBar" }
    /// Stable instance id for the test harness.
    fn id(&self) -> Option<&str> { Some("status-bar") }
    fn bounds(&self) -> Rect { self.bounds }
    fn set_bounds(&mut self, b: Rect) { self.bounds = b; }
    fn children(&self) -> &[Box<dyn Widget>] { &self.children }
    fn children_mut(&mut self) -> &mut Vec<Box<dyn Widget>> { &mut self.children }
    fn h_anchor(&self) -> HAnchor { self.base.h_anchor }
    fn v_anchor(&self) -> VAnchor { self.base.v_anchor }
    fn min_size(&self) -> Size { self.base.min_size }
    fn max_size(&self) -> Size { self.base.max_size }
    fn margin(&self) -> Insets { self.base.margin }
    fn widget_base(&self) -> Option<&WidgetBase> { Some(&self.base) }

    fn layout(&mut self, available: Size) -> Size {
        let h = BAR_HEIGHT;
        self.rebuild();
        self.bounds = Rect::new(0.0, 0.0, available.width, h);
        Size::new(available.width, h)
    }

    /// Keep the host drawing while a storage operation is in flight so the
    /// progress readout advances. This is the visibility-gated channel
    /// (see [`Widget::needs_draw`]), so it also marks retained ancestors
    /// for re-raster without an invalidation-epoch bump.
    fn needs_draw(&self) -> bool {
        self.state.pending_op_count() > 0
    }

    fn paint(&mut self, ctx: &mut dyn DrawCtx) {
        let w = self.bounds.width;
        let h = self.bounds.height;
        if w <= 0.0 || h <= 0.0 { return; }

        ctx.save();
        ctx.clip_rect(0.0, 0.0, w, h);

        let visuals = ctx.visuals();
        // Thin separator above + tinted strip — distinct from canvas + viewport.
        ctx.set_fill_color(visuals.panel_fill);
        ctx.begin_path();
        ctx.rect(0.0, 0.0, w, h);
        ctx.fill();
        ctx.set_stroke_color(visuals.separator);
        ctx.set_line_width(1.0);
        ctx.begin_path();
        ctx.move_to(0.0, h);
        ctx.line_to(w, h);
        ctx.stroke();

        if let Some(font) = font_settings::current_system_font() {
            ctx.set_font(font);
        }
        ctx.set_font_size(FONT_SIZE);
        let dim = visuals.text_dim;
        ctx.set_fill_color(dim);

        // Y baseline: roughly centered vertically.
        let y = h * 0.5 - 4.0;

        // Left: zoom percent + version. canvas_zoom is the canvas
        // widget's pan/zoom scale factor (1.0 = native).
        let zoom_pct = (*self.state.canvas_zoom.lock().unwrap() * 100.0).round() as i64;
        let zoom_str = format!("{}%", zoom_pct);
        ctx.fill_text(&zoom_str, 12.0, y);
        ctx.fill_text(&format!("v{}", APP_VERSION), 80.0, y);

        // Middle: storage activity + cancel, then the sticky notice.
        // Both are absent (and reserve nothing) when idle.
        if let Some(text) = self.activity.clone() {
            ctx.set_fill_color(visuals.text_color);
            ctx.fill_text(&text, STORAGE_X, y);
            if let Some((x0, x1)) = self.cancel_span {
                let glyph = crate::fa::TIMES.to_string();
                let gw = ctx.measure_text(&glyph).map(|m| m.width).unwrap_or(0.0);
                ctx.set_fill_color(dim);
                ctx.fill_text(&glyph, x0 + (x1 - x0 - gw) * 0.5, y);
            }
        }
        if let (Some(notice), Some((x0, _))) = (self.notice.clone(), self.notice_span) {
            let color = match notice.level {
                NoticeLevel::Error => ERROR_COLOR,
                NoticeLevel::Info => visuals.text_color,
            };
            ctx.set_fill_color(color);
            ctx.fill_text(&notice.text, x0, y);
        }

        // Right: node count + "Saved" indicator.
        let g = self.state.graph.lock().unwrap();
        let node_count = g.node_count();
        let noodle_count = g.noodle_count();
        drop(g);
        let saved_label = if self.state.current_file.lock().unwrap().is_some() {
            "Saved".to_string()
        } else {
            "Unsaved".to_string()
        };
        // Right-align estimate.
        let right_text = format!("Nodes: {}    Noodles: {}    {}", node_count, noodle_count, saved_label);
        let est_w = (right_text.chars().count() as f64) * 6.5;
        ctx.set_fill_color(dim);
        ctx.fill_text(&right_text, w - est_w - 12.0, y);

        ctx.restore();
    }

    /// Only the storage affordances are interactive; the rest of the bar
    /// is informational and lets events pass through to other hit-test
    /// layers. `local_pos` is Y-up widget-local, so the vertical test runs
    /// from the bar's bottom edge upward.
    fn hit_test(&self, local_pos: Point) -> bool {
        if local_pos.y < 0.0 || local_pos.y > self.bounds.height {
            return false;
        }
        Self::span_contains(self.cancel_span, local_pos.x)
            || Self::span_contains(self.notice_span, local_pos.x)
    }

    /// The spans are computed in `layout`, so between the queue draining
    /// and the next layout the cancel span is stale. Re-checking
    /// `pending_op_count` keeps a click there from being consumed as a
    /// phantom no-op — with nothing to cancel it should fall through like
    /// any other click on the informational part of the bar.
    fn on_event(&mut self, event: &Event) -> EventResult {
        if let Event::MouseDown {
            pos,
            button: MouseButton::Left,
            ..
        } = event
        {
            if pos.y < 0.0 || pos.y > self.bounds.height {
                return EventResult::Ignored;
            }
            if Self::span_contains(self.cancel_span, pos.x) && self.state.pending_op_count() > 0 {
                self.state.cancel_pending_ops();
                return EventResult::Consumed;
            }
            if Self::span_contains(self.notice_span, pos.x) {
                self.state.dismiss_notice();
                self.notice = None;
                self.notice_span = None;
                return EventResult::Consumed;
            }
        }
        EventResult::Ignored
    }

    /// Surface the storage segment for the inspector and UI tests (read
    /// through the harness's `snapshot()` → `InspectorNode::properties`).
    fn properties(&self) -> Vec<(&'static str, String)> {
        vec![
            ("storage", self.activity.clone().unwrap_or_default()),
            (
                "notice",
                self.notice.as_ref().map(|n| n.text.clone()).unwrap_or_default(),
            ),
            (
                "cancel_center_x",
                self.cancel_center_x()
                    .map(|x| x.to_string())
                    .unwrap_or_default(),
            ),
        ]
    }
}
