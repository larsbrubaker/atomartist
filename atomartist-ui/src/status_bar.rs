//! Bottom status bar — zoom %, version, node count, etc.
//!
//! A custom Widget that paints a thin horizontal strip with text labels
//! pulled from `AppState` each frame. Doesn't use a real Label widget
//! because the values change every frame and rebuilding Labels each
//! paint is wasteful. Width and height are STRETCH × natural baseline
//! so it sits flush at the bottom of the column.

use agg_gui::{
    font_settings, Color, DrawCtx, Event, EventResult, HAnchor, Insets, Point, Rect, Size,
    VAnchor, Widget, WidgetBase,
};

use crate::app_state::AppState;

const BAR_HEIGHT: f64 = 24.0;
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct StatusBar {
    bounds: Rect,
    children: Vec<Box<dyn Widget>>,
    base: WidgetBase,
    state: AppState,
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
        }
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
        self.bounds = Rect::new(0.0, 0.0, available.width, h);
        Size::new(available.width, h)
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
        ctx.set_font_size(11.0);
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
        ctx.fill_text(&right_text, w - est_w - 12.0, y);

        let _ = Color::transparent(); // Color import kept for future use

        ctx.restore();
    }

    fn hit_test(&self, _local_pos: Point) -> bool {
        // Status bar is informational — let events pass through to other
        // hit-test layers (none here, but signaling clearly is good).
        false
    }

    fn on_event(&mut self, _event: &Event) -> EventResult {
        EventResult::Ignored
    }
}
