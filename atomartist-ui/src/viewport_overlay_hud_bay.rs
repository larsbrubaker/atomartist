//! HUD "banana" bay framing for the viewport overlay.
//!
//! Split out of `viewport_overlay.rs` so the main module stays
//! under the repository line-count guardrail. The actual rendering
//! is a pixel-identical port of MatterCAD's `renderRoundedGroup` /
//! `renderRoundedLine` from `View3DWidget.cs:498-552`. Each "bay" is
//! a fat round-capped AGG stroke at radius `cube_r + 12 + width/2`
//! that we then draw a 1-px outline around (via a nested
//! `ConvStroke`) to match the C# code exactly.

use std::f64::consts::TAU;

use agg_gui::{
    Color, DrawCtx, Event, EventResult, HAnchor, Point, Rect, Size, VAnchor, Widget, WidgetBase,
};
use agg_rust::arc::Arc as AggArc;
use agg_rust::basics::{is_close, is_line_to, is_move_to, is_stop, VertexSource};
use agg_rust::conv_stroke::ConvStroke;
use agg_rust::math_stroke::LineCap as AggLineCap;

use super::{
    BOTTOM_ROW_SPACING, BOTTOM_ROW_TOP_OFFSET, CUBE_MARGIN_RIGHT, CUBE_MARGIN_TOP, CUBE_SIZE,
    HUD_BAY_GAP, HUD_STROKE_WIDTH,
};

/// Transparent leaf widget that paints the HUD bay framing on top of
/// the viewport's background fill. Slotted into the overlay's
/// children at index `CHILD_HUD_BAY` so paint order is:
///   1. Viewport (paints its own bg + 3-D content).
///   2. HudBayLayer (paints banana arcs + separator).
///   3. Cube widget (paints into its sub-region).
///   4. Ring buttons.
///   5. Bottom-row buttons / dropdowns.
pub(super) struct HudBayLayer {
    bounds: Rect,
    base: WidgetBase,
    children_storage: Vec<Box<dyn Widget>>,
}

impl HudBayLayer {
    pub(super) fn new() -> Self {
        Self {
            bounds: Rect::default(),
            base: WidgetBase::new()
                .with_h_anchor(HAnchor::STRETCH)
                .with_v_anchor(VAnchor::STRETCH),
            children_storage: Vec::new(),
        }
    }
}

impl Widget for HudBayLayer {
    fn type_name(&self) -> &'static str { "HudBayLayer" }
    fn bounds(&self) -> Rect { self.bounds }
    fn set_bounds(&mut self, b: Rect) { self.bounds = b; }
    fn children(&self) -> &[Box<dyn Widget>] { &[] }
    fn children_mut(&mut self) -> &mut Vec<Box<dyn Widget>> { &mut self.children_storage }
    fn h_anchor(&self) -> HAnchor { self.base.h_anchor }
    fn v_anchor(&self) -> VAnchor { self.base.v_anchor }
    fn widget_base(&self) -> Option<&WidgetBase> { Some(&self.base) }

    fn layout(&mut self, available: Size) -> Size {
        self.bounds = Rect::new(0.0, 0.0, available.width, available.height);
        available
    }

    fn paint(&mut self, ctx: &mut dyn DrawCtx) {
        // HUD background — pixel-identical port of MatterCAD's
        // `renderRoundedGroup` / `renderRoundedLine` from
        // `View3DWidget.cs:498-552`. Each "bay" is a fat round-capped
        // stroke at radius `cube_r + 12 + width/2`.
        let w = self.bounds.width;
        let h = self.bounds.height;
        let cube_cx = w - CUBE_MARGIN_RIGHT - CUBE_SIZE * 0.5;
        let cube_cy = h - CUBE_MARGIN_TOP - CUBE_SIZE * 0.5;
        let cube_r = CUBE_SIZE * 0.5;
        let bay_radius = cube_r + HUD_BAY_GAP + HUD_STROKE_WIDTH * 0.5;
        let stroke_w = HUD_STROKE_WIDTH * 2.0;

        let visuals = ctx.visuals();
        let dark = 0.299 * visuals.bg_color.r
            + 0.587 * visuals.bg_color.g
            + 0.114 * visuals.bg_color.b
            < 0.5;
        // MatterCAD: `hudBackgroundColor = theme.BedBackgroundColor.WithAlpha(120)`
        // (~47% alpha), `hudStrokeColor = theme.TextColor.WithAlpha(120)`.
        let hud_bg = if dark {
            Color::rgba(0.20, 0.21, 0.24, 0.47)
        } else {
            Color::rgba(0.85, 0.87, 0.89, 0.85)
        };
        let hud_outline = if dark {
            Color::rgba(0.85, 0.86, 0.90, 0.47)
        } else {
            Color::rgba(0.20, 0.21, 0.24, 0.47)
        };

        // MatterCAD regions:
        //   renderRoundedGroup(.3, .25)     top tool group
        //   renderRoundedGroup(.1, .6)      home / fit group
        //   renderRoundedGroup(.1, .9)      turntable / perspective group
        //
        // AtomArtist extension:
        //   three single circular regions behind the vertical Bed /
        //   Render / Snap controls, using the same AGG circle/stroke
        //   approach as the group regions.
        //
        // All are rendered through the same AGG Arc -> ConvStroke ->
        // ConvStroke pipeline as the C# code.  The group endpoints
        // below are expressed in the same ring coordinate convention
        // as button placement to avoid long-way arc wrapping.
        paint_ring_span_bay_exact(
            ctx, cube_cx, cube_cy, bay_radius,
            TAU * 0.15, -TAU * 0.15,
            stroke_w, hud_bg, hud_outline,
        );
        paint_ring_span_bay_exact(
            ctx, cube_cx, cube_cy, bay_radius,
            TAU * 0.40, TAU * 0.30,
            stroke_w, hud_bg, hud_outline,
        );
        paint_ring_span_bay_exact(
            ctx, cube_cx, cube_cy, bay_radius,
            -TAU * 0.30, -TAU * 0.40,
            stroke_w, hud_bg, hud_outline,
        );

        let cube_y = h - CUBE_MARGIN_TOP - CUBE_SIZE;
        let base_below = BOTTOM_ROW_TOP_OFFSET - CUBE_MARGIN_TOP - CUBE_SIZE;
        for dy_below in [
            base_below,
            base_below + BOTTOM_ROW_SPACING,
            base_below + BOTTOM_ROW_SPACING * 2.0,
        ] {
            paint_circle_bay_exact(
                ctx,
                cube_cx,
                cube_y - dy_below,
                stroke_w * 0.5,
                hud_bg,
                hud_outline,
            );
        }
    }

    fn on_event(&mut self, _: &Event) -> EventResult { EventResult::Ignored }

    /// Purely decorative: the bay paints the HUD framing arcs but never
    /// handles input. It stretches to fill the whole overlay, so the
    /// default bounds-based `hit_test` would claim every pixel over the
    /// 3-D viewport — and since the bay is stacked ABOVE the viewport in
    /// `ViewportOverlay`, that stole pointer capture from a viewport
    /// mid-drag (it's a later sibling, so hit-testing reaches it before
    /// the viewport's `claims_pointer_exclusively`). Returning `false`
    /// makes the bay transparent to hit-testing, so clicks + drags fall
    /// through to the viewport beneath it.
    fn hit_test(&self, _local_pos: Point) -> bool { false }
}

/// Render one HUD group with the same AGG math MatterCAD uses:
///
/// ```csharp
/// var arc = new Arc(tumbleCubeCenter, radius, start, end);
/// var background = new Stroke(arc, width * 2);
/// background.LineCap = LineCap.Round;
/// Render(background, hudBackgroundColor);
/// Render(new Stroke(background, scale), hudStrokeColor);
/// ```
///
/// The start/end arguments are in the same **ring coordinates** as
/// `ViewportOverlay::layout` uses for button placement:
///
/// ```text
/// angle 0        = straight up from cube centre
/// positive angle = toward the left/top side
/// negative angle = toward the right/top side
/// ```
///
/// The top group is therefore Select(+Tau*.15) to Zoom(-Tau*.15).
/// Internally those ring angles are converted to AGG's standard
/// convention (0 = +X, CCW positive) before feeding `agg_rust::arc::Arc`.
fn paint_ring_span_bay_exact(
    ctx: &mut dyn DrawCtx,
    cx: f64,
    cy: f64,
    radius: f64,
    ring_start: f64,
    ring_end: f64,
    stroke_width: f64,
    fill: Color,
    outline: Color,
) {
    let to_agg_angle = |ring_angle: f64| std::f64::consts::FRAC_PI_2 + ring_angle;
    // We want the short top arc from right→left through the top.
    // In AGG coordinates: Zoom(-.15) maps to ~0.10τ, Select(+.15)
    // maps to ~0.40τ.  CCW from zoom to select is exactly the top
    // group; reversing these endpoints draws the long way around.
    let a_start = to_agg_angle(ring_end);
    let a_end = to_agg_angle(ring_start);
    let arc = AggArc::new(cx, cy, radius, radius, a_start, a_end, true);

    let mut background = ConvStroke::new(arc);
    background.set_width(stroke_width);
    background.set_line_cap(AggLineCap::Round);
    fill_vertex_source(ctx, &mut background, fill);

    // C# does `new Stroke(background, scale)` and renders that as
    // the outline. Recreate the background stroke from a fresh arc so
    // the nested stroke consumes the same source geometry from the
    // start.
    let arc = AggArc::new(cx, cy, radius, radius, a_start, a_end, true);
    let mut background = ConvStroke::new(arc);
    background.set_width(stroke_width);
    background.set_line_cap(AggLineCap::Round);
    let mut border = ConvStroke::new(background);
    border.set_width(1.0);
    fill_vertex_source(ctx, &mut border, outline);
}

/// Render a single circular HUD region with the same AGG machinery as
/// the arc groups. The circle itself is an AGG full-circle arc filled
/// as the translucent region; the border is `ConvStroke(circle, 1)`,
/// matching the C# `new Stroke(background, scale)` idea for a simple
/// closed curve.
fn paint_circle_bay_exact(
    ctx: &mut dyn DrawCtx,
    cx: f64,
    cy: f64,
    radius: f64,
    fill: Color,
    outline: Color,
) {
    let mut circle = AggArc::new(cx, cy, radius, radius, 0.0, TAU, true);
    fill_vertex_source(ctx, &mut circle, fill);

    let circle = AggArc::new(cx, cy, radius, radius, 0.0, TAU, true);
    let mut border = ConvStroke::new(circle);
    border.set_width(1.0);
    fill_vertex_source(ctx, &mut border, outline);
}

/// Feed an AGG [`VertexSource`] into `DrawCtx` as a filled polygon.
/// This lets us use agg-rust's actual `Arc` and `ConvStroke`
/// generators while still drawing through agg-gui's backend-agnostic
/// `DrawCtx` interface.
fn fill_vertex_source(ctx: &mut dyn DrawCtx, source: &mut dyn VertexSource, color: Color) {
    source.rewind(0);
    ctx.set_fill_color(color);
    ctx.begin_path();
    let mut first: Option<(f64, f64)> = None;
    loop {
        let mut x = 0.0;
        let mut y = 0.0;
        let cmd = source.vertex(&mut x, &mut y);
        if is_stop(cmd) {
            if let Some((fx, fy)) = first {
                ctx.line_to(fx, fy);
            }
            break;
        }
        if is_move_to(cmd) {
            ctx.move_to(x, y);
            first = Some((x, y));
        } else if is_line_to(cmd) {
            ctx.line_to(x, y);
        } else if is_close(cmd) {
            if let Some((fx, fy)) = first {
                ctx.line_to(fx, fy);
            }
        }
    }
    ctx.fill();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bay fills the whole overlay but is decorative only. It must
    /// be transparent to hit-testing so it doesn't steal pointer
    /// capture from the 3-D viewport it's stacked on top of — the
    /// capture theft that left a body-drag's mouse-up routed away from
    /// the viewport, gluing the body to the cursor.
    #[test]
    fn bay_never_claims_pointer_hits() {
        let mut bay = HudBayLayer::new();
        bay.layout(Size::new(800.0, 600.0));
        // Several points across the full-screen bay, including its
        // centre and corners — none may claim the hit.
        for &(x, y) in &[(0.0, 0.0), (400.0, 300.0), (799.0, 599.0), (50.0, 550.0)] {
            assert!(
                !bay.hit_test(Point::new(x, y)),
                "decorative bay must not hit-test true at ({x}, {y})",
            );
        }
    }
}
