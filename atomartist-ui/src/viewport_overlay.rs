//! Top-right HUD that wraps `Viewport3dWidget` with the tumble cube and
//! a ring of navigation buttons — port of MatterCAD's
//! `View3DWidget.CreateTumbleCubeAndControls`.
//!
//! Layout:
//!
//! ```text
//!                                      ┌─────────────┐
//!                                      │   ┌─────┐   │  ← cube + ring buttons
//!  ┌─────────────────────────────────┐ │   │     │   │
//!  │                                 │ │   └─────┘   │  ← Home / Sel / Tools
//!  │       3-D viewport              │ │             │
//!  │                                 │ │  [Style ▼]  │  ← render-style picker
//!  │                                 │ │             │
//!  └─────────────────────────────────┘ └─────────────┘
//! ```
//!
//! The container itself is a `Widget` that paints the HUD arcs and lets
//! its children draw on top.  It deliberately doesn't use the existing
//! `Stack` because the buttons need pixel-precise top-right anchoring
//! against a rotating polar layout (`RotatedMargin` in the C# source),
//! not the "fill the parent" stretch `Stack` does.

use std::f64::consts::TAU;
use std::sync::Arc;

use agg_gui::{
    text::Font, Color, DrawCtx, Event, EventResult, HAnchor, LineCap, MouseButton, Point, Rect,
    Size, VAnchor, Widget, WidgetBase,
};
use atomartist_renderer::{
    OrbitMode, Projection, RenderStyle, TumbleCubeInputs, TumbleCubeWidget, Viewport3dWidget,
    ViewportInputs, ViewportTool,
};

use crate::app_state::AppState;
use crate::circular_dropdown::{CircularDropdown, DropdownItem};
use crate::circular_icon_button::CircularIconButton;
use crate::icons::IconKind;

/// Pixel side of the tumble cube widget itself (radius from centre is
/// `CUBE_SIZE / 2`).  Matches MatterCAD's `100 * DeviceScale` in
/// `TumbleCubeControl::TumbleCubeControl`.
const CUBE_SIZE: f64 = 100.0;
/// Distance from the cube centre to button centres on the ring.
/// Matches MatterCAD's `radius = 70 * DeviceScale` in
/// `View3DWidget.RotatedMargin`.
const RING_RADIUS: f64 = 70.0;
/// Right / top margin of the cube within the overlay — matches
/// MatterCAD's `Margin = new BorderDouble(0, 0, 40, 45)`.
const CUBE_MARGIN_RIGHT: f64 = 40.0;
const CUBE_MARGIN_TOP: f64 = 45.0;
/// Side of every ring button.  MatterCAD's PNG icons are 16 px and
/// `ThemedRadioIconButton` adds roughly 8 px theme padding around them
/// for the circular fill, giving an effective ~32 px button.
const BUTTON_SIZE: f64 = 32.0;
/// HUD bay stroke width (matches MatterCAD's `width = 17 * scale`).
const HUD_STROKE_WIDTH: f64 = 17.0;
/// Radial gap between the cube edge and the bay's stroke centerline.
/// Matches MatterCAD's `tumbleCubeRadius + 12 * scale + width / 2`.
const HUD_BAY_GAP: f64 = 12.0;
/// Vertical drop of the small horizontal separator below the cube —
/// `renderRoundedLine(18, 101)` in MatterCAD.  Measured from the
/// cube's centre.
const HUD_SEPARATOR_BELOW_CENTER: f64 = 101.0;
/// Half-length of the horizontal separator line — matches the
/// `lineWidth` argument of MatterCAD's `renderRoundedLine(18, 101)`
/// call.
const HUD_SEPARATOR_HALF_LEN: f64 = 18.0;
/// Y-distance from the overlay's top edge to the FIRST bottom-row
/// button's centre.  Matches MatterCAD's `var startHeight = 180`.
const BOTTOM_ROW_TOP_OFFSET: f64 = 180.0;
/// Vertical spacing between rows in the bottom column. Matches
/// MatterCAD's `var ySpacing = 40`.
const BOTTOM_ROW_SPACING: f64 = 40.0;

/// Build the overlay container.  The returned widget is what
/// `top_level.rs` mounts in place of the bare `Viewport3dWidget`.
pub fn build_viewport_overlay(state: AppState, font: Arc<Font>) -> Box<dyn Widget> {
    let viewport_inputs = ViewportInputs {
        last_mesh_output: state.last_mesh_output.clone(),
        display_node: state.display_node.clone(),
        selection: state.selection.clone(),
        camera: state.camera.clone(),
        tool: state.viewport_tool.clone(),
        render_style: state.render_style.clone(),
        show_bed: state.show_bed.clone(),
    };
    let cube_inputs = TumbleCubeInputs {
        camera: state.camera.clone(),
    };

    let viewport = Box::new(Viewport3dWidget::new(viewport_inputs));
    let cube = Box::new(TumbleCubeWidget::new(cube_inputs));

    let mut overlay = ViewportOverlay::new(viewport, cube);

    // Ring buttons. Positions match MatterCAD's `RotatedMargin` call
    // sites in `CreateTumbleCubeAndControls`:
    //   Tau * .30 → Home (down-left)
    //   Tau * .40 → Zoom-to-Selection (further down-left)
    //   Tau * .15 → PartSelect
    //   Tau * .05 → Rotate
    //  -Tau * .05 → Translate (Pan)
    //  -Tau * .15 → Scale (Zoom)
    //  -Tau * .30 → Perspective (right of cube)
    //  -Tau * .40 → Turntable (further right)
    add_tool_button(&mut overlay, &state, TAU * 0.15, IconKind::Select, ViewportTool::Select);
    add_tool_button(&mut overlay, &state, TAU * 0.05, IconKind::Rotate, ViewportTool::Rotate);
    add_tool_button(&mut overlay, &state, -TAU * 0.05, IconKind::Pan, ViewportTool::Pan);
    add_tool_button(&mut overlay, &state, -TAU * 0.15, IconKind::Zoom, ViewportTool::Zoom);
    add_home_button(&mut overlay, &state, TAU * 0.30);
    add_zoom_to_sel_button(&mut overlay, &state, TAU * 0.40);
    add_turntable_button(&mut overlay, &state, -TAU * 0.40);
    add_perspective_button(&mut overlay, &state, -TAU * 0.30);

    add_bottom_row(&mut overlay, &state, &font);

    Box::new(overlay)
}

/// Placement rule for an overlay child past the viewport + cube.
#[derive(Clone, Copy, Debug)]
enum Placement {
    /// Centre on the ring at the given polar angle (radians, 0 = up).
    Ring { angle: f64, size: Size },
    /// Centre at `(cube_cx + dx, cube_cy_top - dy)` — used by the
    /// render-style picker beneath the cube.
    BelowCube { dx: f64, dy_below: f64, size: Size },
}

/// Custom container — does the polar button layout MatterCAD does in
/// `View3DWidget.MakeRoundAndAdd`.  The first child is the viewport
/// (stretched to fill), the second is the cube (anchored top-right),
/// and the rest are positioned by `placements[child_idx - 2]`.
pub struct ViewportOverlay {
    bounds: Rect,
    children: Vec<Box<dyn Widget>>,
    base: WidgetBase,
    placements: Vec<Placement>,
}

impl ViewportOverlay {
    pub fn new(viewport: Box<dyn Widget>, cube: Box<dyn Widget>) -> Self {
        Self {
            bounds: Rect::default(),
            children: vec![viewport, cube],
            base: WidgetBase::new()
                .with_h_anchor(HAnchor::STRETCH)
                .with_v_anchor(VAnchor::STRETCH),
            placements: Vec::new(),
        }
    }

    /// Push a ring button at the given polar angle (in radians).
    /// `angle = 0` is straight above the cube; positive rotates
    /// counter-clockwise, matching MatterCAD's `MathHelper.Tau * .X`
    /// values which are angles in the standard math sense.
    pub fn add_ring_button(&mut self, w: Box<dyn Widget>, angle: f64) {
        self.children.push(w);
        self.placements.push(Placement::Ring {
            angle,
            size: Size::new(BUTTON_SIZE, BUTTON_SIZE),
        });
    }

    /// Push a widget centred under the cube at the given `(dx, y_below)`
    /// offset.  `dx` is horizontal pixels relative to the cube centre,
    /// `y_below` is how far below the cube's bottom edge to place the
    /// widget centre (in Y-up coords).
    pub fn add_below_cube(&mut self, w: Box<dyn Widget>, dx: f64, y_below: f64, size: Size) {
        self.children.push(w);
        self.placements.push(Placement::BelowCube { dx, dy_below: y_below, size });
    }
}

impl Widget for ViewportOverlay {
    fn type_name(&self) -> &'static str { "ViewportOverlay" }
    fn bounds(&self) -> Rect { self.bounds }
    fn set_bounds(&mut self, b: Rect) { self.bounds = b; }
    fn children(&self) -> &[Box<dyn Widget>] { &self.children }
    fn children_mut(&mut self) -> &mut Vec<Box<dyn Widget>> { &mut self.children }
    fn h_anchor(&self) -> HAnchor { self.base.h_anchor }
    fn v_anchor(&self) -> VAnchor { self.base.v_anchor }
    fn widget_base(&self) -> Option<&WidgetBase> { Some(&self.base) }

    fn layout(&mut self, available: Size) -> Size {
        self.bounds = Rect::new(0.0, 0.0, available.width, available.height);
        let cube_w = CUBE_SIZE;
        let cube_h = CUBE_SIZE;
        let cube_x = (available.width - CUBE_MARGIN_RIGHT - cube_w).max(0.0);
        // agg-gui is Y-up: top of widget is `available.height`.  Cube
        // sits CUBE_MARGIN_TOP below the top edge.
        let cube_y_top = available.height - CUBE_MARGIN_TOP;
        let cube_y = (cube_y_top - cube_h).max(0.0);
        let cube_cx = cube_x + cube_w * 0.5;
        let cube_cy = cube_y + cube_h * 0.5;

        // children[0]: viewport — stretches to fill.
        if let Some(vp) = self.children.get_mut(0) {
            vp.layout(available);
            vp.set_bounds(Rect::new(0.0, 0.0, available.width, available.height));
        }
        // children[1]: tumble cube — fixed-size square anchored top-right.
        if let Some(cube) = self.children.get_mut(1) {
            cube.layout(Size::new(cube_w, cube_h));
            cube.set_bounds(Rect::new(cube_x, cube_y, cube_w, cube_h));
        }

        for (i, placement) in self.placements.iter().enumerate() {
            let Some(child) = self.children.get_mut(2 + i) else { continue };
            match *placement {
                Placement::Ring { angle, size } => {
                    // 0 = up, CCW positive — translate to widget-local
                    // pixel offset from the cube centre.  Y-up: "up"
                    // means +Y; sin/cos give the offset.
                    let dx = -angle.sin() * RING_RADIUS;
                    let dy = angle.cos() * RING_RADIUS;
                    child.layout(size);
                    child.set_bounds(Rect::new(
                        cube_cx + dx - size.width * 0.5,
                        cube_cy + dy - size.height * 0.5,
                        size.width,
                        size.height,
                    ));
                }
                Placement::BelowCube { dx, dy_below, size } => {
                    // Below cube = lower Y in Y-up coords.
                    let cx = cube_cx + dx;
                    let cy = cube_y - dy_below;
                    child.layout(size);
                    child.set_bounds(Rect::new(
                        cx - size.width * 0.5,
                        cy - size.height * 0.5,
                        size.width,
                        size.height,
                    ));
                }
            }
        }

        available
    }

    fn paint(&mut self, ctx: &mut dyn DrawCtx) {
        // HUD background — pixel-identical port of MatterCAD's
        // `renderRoundedGroup` / `renderRoundedLine` from
        // `View3DWidget.cs:498-552`.  Each "bay" is a fat stroke
        // (width = HUD_STROKE_WIDTH * 2 because MatterCAD multiplies
        // its declared width by 2 inside `RenderPath`) along an arc
        // with round line caps.  A second, thinner stroke on top
        // gives the bay a 1-px outline.
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
        // MatterCAD's colours: `hudBackgroundColor = theme.BedBackgroundColor.WithAlpha(120)`
        // (~47% alpha) and `hudStrokeColor = theme.TextColor.WithAlpha(120)`.
        let hud_bg = if dark {
            Color::rgba(0.20, 0.21, 0.24, 0.47)
        } else {
            Color::rgba(0.94, 0.95, 0.96, 0.47)
        };
        let hud_outline = if dark {
            Color::rgba(0.85, 0.86, 0.90, 0.47)
        } else {
            Color::rgba(0.20, 0.21, 0.24, 0.47)
        };

        // Three ring bays — angles match MatterCAD's
        // `renderRoundedGroup(.3, .25)`, `(.1, .6)`, `(.1, .9)`.
        // MatterCAD's angle convention is standard math: angle 0 =
        // +X (3 o'clock), CCW positive; agg-gui uses the same.
        paint_arc_bay(
            ctx,
            cube_cx, cube_cy, bay_radius,
            TAU * 0.25, TAU * 0.30,
            stroke_w, hud_bg, hud_outline,
        );
        paint_arc_bay(
            ctx,
            cube_cx, cube_cy, bay_radius,
            TAU * 0.60, TAU * 0.10,
            stroke_w, hud_bg, hud_outline,
        );
        paint_arc_bay(
            ctx,
            cube_cx, cube_cy, bay_radius,
            TAU * 0.90, TAU * 0.10,
            stroke_w, hud_bg, hud_outline,
        );

        // Small horizontal separator below the cube — MatterCAD's
        // `renderRoundedLine(18, 101)`.  `heightBelowCenter = 101`
        // is measured downward from the cube centre in Y-down screen
        // coords; we negate it for Y-up.
        let sep_y = cube_cy - HUD_SEPARATOR_BELOW_CENTER;
        paint_line_bay(
            ctx,
            cube_cx - HUD_SEPARATOR_HALF_LEN,
            sep_y,
            cube_cx + HUD_SEPARATOR_HALF_LEN,
            sep_y,
            stroke_w,
            hud_bg,
            hud_outline,
        );
    }

    fn on_event(&mut self, event: &Event) -> EventResult {
        // Reverse paint order: last-added child is on top, so it gets
        // first crack at the event.
        for child in self.children.iter_mut().rev() {
            if let Some(local_event) = translate_for_child(event, child.bounds()) {
                let result = child.on_event(&local_event);
                if result == EventResult::Consumed {
                    return EventResult::Consumed;
                }
            }
        }
        EventResult::Ignored
    }
}

/// Translate a parent-space event into a child-space event.  Returns
/// `None` for MouseDown / MouseWheel that miss the child; MouseMove
/// and MouseUp are always forwarded so widgets can clear hover state
/// and terminate drags cleanly.
fn translate_for_child(event: &Event, child_bounds: Rect) -> Option<Event> {
    let translate = |p: Point| Point::new(p.x - child_bounds.x, p.y - child_bounds.y);
    let contains = |p: Point| {
        p.x >= child_bounds.x
            && p.x < child_bounds.x + child_bounds.width
            && p.y >= child_bounds.y
            && p.y < child_bounds.y + child_bounds.height
    };
    match event {
        Event::MouseDown { pos, button, modifiers } => {
            if contains(*pos) {
                Some(Event::MouseDown {
                    pos: translate(*pos),
                    button: *button,
                    modifiers: *modifiers,
                })
            } else {
                None
            }
        }
        Event::MouseUp { pos, button, modifiers } => Some(Event::MouseUp {
            pos: translate(*pos),
            button: *button,
            modifiers: *modifiers,
        }),
        Event::MouseMove { pos } => Some(Event::MouseMove { pos: translate(*pos) }),
        Event::MouseWheel { pos, delta_y, delta_x, modifiers } => {
            if contains(*pos) {
                Some(Event::MouseWheel {
                    pos: translate(*pos),
                    delta_y: *delta_y,
                    delta_x: *delta_x,
                    modifiers: *modifiers,
                })
            } else {
                None
            }
        }
        other => Some(other.clone()),
    }
}

/// Strokes a curved arc with a fat round-capped line, then overlays a
/// thin 1-px outline.  This is the pixel-exact port of MatterCAD's
/// `RenderPath` + `renderRoundedGroup` (`View3DWidget.cs:506-526`):
///
/// ```csharp
/// var background = new Stroke(vertexSource, width * 2);
/// background.LineCap = LineCap.Round;
/// Render(background, hudBackgroundColor);
/// Render(new Stroke(background, scale), hudStrokeColor);
/// ```
///
/// `center_angle` and `half_span` are in MatterCAD's convention
/// (standard math: 0 = +X, CCW positive, radians).  The arc sweeps
/// from `center - half_span` to `center + half_span`.
fn paint_arc_bay(
    ctx: &mut dyn DrawCtx,
    cx: f64,
    cy: f64,
    radius: f64,
    center_angle: f64,
    half_span: f64,
    stroke_width: f64,
    fill: Color,
    outline: Color,
) {
    let a_start = center_angle - half_span;
    let a_end = center_angle + half_span;
    let steps = 32;

    // First pass: fat fill stroke with round caps.
    ctx.set_stroke_color(fill);
    ctx.set_line_width(stroke_width);
    ctx.set_line_cap(LineCap::Round);
    ctx.begin_path();
    for i in 0..=steps {
        let t = i as f64 / steps as f64;
        let a = a_start + (a_end - a_start) * t;
        let x = cx + radius * a.cos();
        let y = cy + radius * a.sin();
        if i == 0 {
            ctx.move_to(x, y);
        } else {
            ctx.line_to(x, y);
        }
    }
    ctx.stroke();

    // Second pass: thin outline.  We don't have a direct
    // stroke-of-a-stroke primitive, so we approximate MatterCAD's
    // `new Stroke(background, scale)` (a 1-px outline around the
    // fat stroke) by tracing the two parallel edges of the fat
    // stroke as 1-px lines.  Round caps at the ends are
    // approximated by the cap arcs.
    let half = stroke_width * 0.5;
    ctx.set_stroke_color(outline);
    ctx.set_line_width(1.0);
    // Outer parallel curve.
    ctx.begin_path();
    for i in 0..=steps {
        let t = i as f64 / steps as f64;
        let a = a_start + (a_end - a_start) * t;
        let x = cx + (radius + half) * a.cos();
        let y = cy + (radius + half) * a.sin();
        if i == 0 {
            ctx.move_to(x, y);
        } else {
            ctx.line_to(x, y);
        }
    }
    ctx.stroke();
    // Inner parallel curve.
    ctx.begin_path();
    for i in 0..=steps {
        let t = i as f64 / steps as f64;
        let a = a_start + (a_end - a_start) * t;
        let x = cx + (radius - half) * a.cos();
        let y = cy + (radius - half) * a.sin();
        if i == 0 {
            ctx.move_to(x, y);
        } else {
            ctx.line_to(x, y);
        }
    }
    ctx.stroke();
    // End-cap outline arcs — semicircles around each tip.
    let cap_steps = 12;
    for tip_angle in [a_start, a_end] {
        let tip_x = cx + radius * tip_angle.cos();
        let tip_y = cy + radius * tip_angle.sin();
        let tangent_x = -tip_angle.sin();
        let tangent_y = tip_angle.cos();
        let perp_x = tip_angle.cos();
        let perp_y = tip_angle.sin();
        // Sweep direction: arcs at `a_start` open outward (away from
        // the bay's interior), arcs at `a_end` likewise.  Use a half
        // turn centred on the tip.
        let outward_sign = if tip_angle == a_start { -1.0 } else { 1.0 };
        ctx.begin_path();
        for i in 0..=cap_steps {
            let t = i as f64 / cap_steps as f64;
            let theta = std::f64::consts::PI * t - std::f64::consts::FRAC_PI_2;
            let lateral = half * theta.cos();
            let along = half * theta.sin() * outward_sign;
            let x = tip_x + perp_x * lateral + tangent_x * along;
            let y = tip_y + perp_y * lateral + tangent_y * along;
            if i == 0 {
                ctx.move_to(x, y);
            } else {
                ctx.line_to(x, y);
            }
        }
        ctx.stroke();
    }
}

/// Stroke a straight-line bay with the same fat-round-capped
/// treatment as [`paint_arc_bay`].  Used for MatterCAD's
/// `renderRoundedLine` separator below the cube.
fn paint_line_bay(
    ctx: &mut dyn DrawCtx,
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    stroke_width: f64,
    fill: Color,
    outline: Color,
) {
    // Fat fill stroke with round caps.
    ctx.set_stroke_color(fill);
    ctx.set_line_width(stroke_width);
    ctx.set_line_cap(LineCap::Round);
    ctx.begin_path();
    ctx.move_to(x0, y0);
    ctx.line_to(x1, y1);
    ctx.stroke();

    // Thin outline traced along the two parallel edges of the fat
    // stroke + semicircle end caps.
    let half = stroke_width * 0.5;
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len = (dx * dx + dy * dy).sqrt().max(1e-6);
    let nx = -dy / len; // unit normal
    let ny = dx / len;
    ctx.set_stroke_color(outline);
    ctx.set_line_width(1.0);
    // Top edge.
    ctx.begin_path();
    ctx.move_to(x0 + nx * half, y0 + ny * half);
    ctx.line_to(x1 + nx * half, y1 + ny * half);
    ctx.stroke();
    // Bottom edge.
    ctx.begin_path();
    ctx.move_to(x0 - nx * half, y0 - ny * half);
    ctx.line_to(x1 - nx * half, y1 - ny * half);
    ctx.stroke();
    // End caps as semicircles.
    let cap_steps = 12;
    let tx = dx / len;
    let ty = dy / len;
    for (cx, cy, sign) in [(x0, y0, -1.0_f64), (x1, y1, 1.0_f64)] {
        ctx.begin_path();
        for i in 0..=cap_steps {
            let t = i as f64 / cap_steps as f64;
            let theta = std::f64::consts::PI * t - std::f64::consts::FRAC_PI_2;
            let lateral = half * theta.cos();
            let along = half * theta.sin() * sign;
            let x = cx + nx * lateral + tx * along;
            let y = cy + ny * lateral + ty * along;
            if i == 0 {
                ctx.move_to(x, y);
            } else {
                ctx.line_to(x, y);
            }
        }
        ctx.stroke();
    }
}


// ---------------------------------------------------------------------------
// Button helpers — every ring button is a `CircularIconButton` so the
// HUD matches MatterCAD's circular `ThemedRadioIconButton` aesthetic
// instead of agg-gui's rectangular `Button` chrome.
// ---------------------------------------------------------------------------

fn add_tool_button(
    overlay: &mut ViewportOverlay,
    state: &AppState,
    angle: f64,
    icon: IconKind,
    tool: ViewportTool,
) {
    let tool_w = state.viewport_tool.clone();
    let tool_r = state.viewport_tool.clone();
    let btn = CircularIconButton::new(icon)
        .with_active_fn(move || *tool_r.lock().unwrap() == tool)
        .on_click(move || {
            *tool_w.lock().unwrap() = tool;
        });
    overlay.add_ring_button(Box::new(btn), angle);
}

fn add_home_button(overlay: &mut ViewportOverlay, state: &AppState, angle: f64) {
    let camera = state.camera.clone();
    let btn = CircularIconButton::new(IconKind::Home)
        .with_tooltip("Reset view")
        .on_click(move || {
            camera.lock().unwrap().reset_view();
        });
    overlay.add_ring_button(Box::new(btn), angle);
}

fn add_zoom_to_sel_button(overlay: &mut ViewportOverlay, state: &AppState, angle: f64) {
    let mesh_slot = state.last_mesh_output.clone();
    let camera = state.camera.clone();
    let btn = CircularIconButton::new(IconKind::Fit)
        .with_tooltip("Zoom to selection")
        .on_click(move || {
            let Some(mesh) = mesh_slot.lock().unwrap().clone() else { return };
            if mesh.num_prop < 3 || mesh.vert_properties.is_empty() {
                return;
            }
            let stride = mesh.num_prop as usize;
            let n = mesh.vert_properties.len() / stride;
            let mut mn = [f32::INFINITY; 3];
            let mut mx = [f32::NEG_INFINITY; 3];
            for i in 0..n {
                for k in 0..3 {
                    let v = mesh.vert_properties[i * stride + k];
                    if v < mn[k] { mn[k] = v; }
                    if v > mx[k] { mx[k] = v; }
                }
            }
            if mn[0].is_finite() && mx[0].is_finite() {
                camera.lock().unwrap().fit_to_bounds(mn, mx);
            }
        });
    overlay.add_ring_button(Box::new(btn), angle);
}

fn add_turntable_button(overlay: &mut ViewportOverlay, state: &AppState, angle: f64) {
    let camera_w = state.camera.clone();
    let setting_w = state.turntable.clone();
    let setting_r = state.turntable.clone();
    let btn = CircularIconButton::new(IconKind::Turn)
        .with_tooltip("Turntable mode")
        .with_active_fn(move || *setting_r.lock().unwrap())
        .on_click(move || {
            let mut s = setting_w.lock().unwrap();
            *s = !*s;
            camera_w.lock().unwrap().orbit_mode = if *s {
                OrbitMode::Turntable
            } else {
                OrbitMode::Trackball
            };
        });
    overlay.add_ring_button(Box::new(btn), angle);
}

fn add_perspective_button(overlay: &mut ViewportOverlay, state: &AppState, angle: f64) {
    let camera_w = state.camera.clone();
    let setting_w = state.perspective.clone();
    let setting_r = state.perspective.clone();
    let btn = CircularIconButton::new(IconKind::Persp)
        .with_tooltip("Perspective mode")
        .with_active_fn(move || *setting_r.lock().unwrap())
        .on_click(move || {
            let mut s = setting_w.lock().unwrap();
            *s = !*s;
            camera_w.lock().unwrap().projection = if *s {
                Projection::Perspective
            } else {
                Projection::Orthographic
            };
        });
    overlay.add_ring_button(Box::new(btn), angle);
}

/// Three circles beneath the cube — Bed toggle, Render-mode dropdown,
/// Snap-amount dropdown — arranged as a **vertical stack** centred
/// under the cube.  Mirrors MatterCAD's layout (`startHeight = 180`,
/// `ySpacing = 40`) where the bed / view-style / grid-snap controls
/// sit at successive rows below the cube.
///
/// `dy_below` values are measured from the cube's bottom edge in
/// Y-up coords:
///   - cube top = `CUBE_MARGIN_TOP` from the overlay's top
///   - cube bottom = `CUBE_MARGIN_TOP + CUBE_SIZE` from the top
///   - bed row centred at `BOTTOM_ROW_TOP_OFFSET` from the top, so
///     it sits `BOTTOM_ROW_TOP_OFFSET - CUBE_MARGIN_TOP - CUBE_SIZE`
///     below the cube bottom (== 35 with the default constants).
fn add_bottom_row(overlay: &mut ViewportOverlay, state: &AppState, font: &Arc<Font>) {
    let base_below = BOTTOM_ROW_TOP_OFFSET - CUBE_MARGIN_TOP - CUBE_SIZE;

    // Row 1 — Bed toggle (hide / show the floor grid).
    {
        let bed_w = state.show_bed.clone();
        let bed_r = state.show_bed.clone();
        let btn = CircularIconButton::new(IconKind::Bed)
            .with_tooltip("Show / hide bed grid")
            .with_active_fn(move || *bed_r.lock().unwrap())
            .on_click(move || {
                let mut b = bed_w.lock().unwrap();
                *b = !*b;
            });
        overlay.add_below_cube(
            Box::new(btn),
            0.0,
            base_below,
            Size::new(BUTTON_SIZE, BUTTON_SIZE),
        );
    }

    // Row 2 — Render-mode (Shader) dropdown.
    {
        let items = vec![
            DropdownItem { label: "Shaded".into(), value: RenderStyle::Shaded },
            DropdownItem { label: "Outline".into(), value: RenderStyle::OutlineOnly },
            DropdownItem { label: "Wireframe".into(), value: RenderStyle::Wireframe },
        ];
        let drop = CircularDropdown::new(
            IconKind::Shade,
            items,
            state.render_style.clone(),
            font.clone(),
        );
        overlay.add_below_cube(
            Box::new(drop),
            0.0,
            base_below + BOTTOM_ROW_SPACING,
            Size::new(BUTTON_SIZE, BUTTON_SIZE),
        );
    }

    // Row 3 — Snap-amount dropdown.
    {
        let items = vec![
            DropdownItem { label: "1".into(), value: 1.0_f64 },
            DropdownItem { label: "5".into(), value: 5.0 },
            DropdownItem { label: "10".into(), value: 10.0 },
            DropdownItem { label: "50".into(), value: 50.0 },
            DropdownItem { label: "100".into(), value: 100.0 },
        ];
        let drop = CircularDropdown::new(
            IconKind::Snap,
            items,
            state.snap_amount.clone(),
            font.clone(),
        );
        overlay.add_below_cube(
            Box::new(drop),
            0.0,
            base_below + BOTTOM_ROW_SPACING * 2.0,
            Size::new(BUTTON_SIZE, BUTTON_SIZE),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use agg_gui::Modifiers;

    /// Bundled NotoSans bytes — needed by `CircularDropdown::new`.
    const FONT_BYTES: &[u8] = include_bytes!(
        "../../../agg-gui/agg-gui/assets/fonts/NotoSans-Regular.ttf"
    );

    fn make_font() -> Arc<Font> {
        agg_gui::font_settings::current_system_font().unwrap_or_else(|| {
            Arc::new(Font::from_bytes(FONT_BYTES.to_vec()).expect("bundled NotoSans"))
        })
    }

    fn fresh_state() -> AppState {
        AppState::new(
            atomartist_lib::Graph::new(),
            atomartist_lib::registry::NodeRegistry::new(),
        )
    }

    /// Click at the given parent-local position. Synthetic
    /// MouseDown + MouseUp pair, modifiers = none.
    fn click_at(overlay: &mut ViewportOverlay, pos: Point) {
        overlay.on_event(&Event::MouseDown {
            pos,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        });
        overlay.on_event(&Event::MouseUp {
            pos,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        });
    }

    fn build_at_size(state: AppState, w: f64, h: f64) -> Box<dyn Widget> {
        let mut overlay = build_viewport_overlay(state, make_font());
        overlay.layout(Size::new(w, h));
        overlay
    }

    #[test]
    fn overlay_constructs_with_minimum_children() {
        let state = fresh_state();
        let viewport_inputs = ViewportInputs::empty();
        let cube_inputs = TumbleCubeInputs { camera: state.camera.clone() };
        let viewport = Box::new(Viewport3dWidget::new(viewport_inputs));
        let cube = Box::new(TumbleCubeWidget::new(cube_inputs));
        let overlay = ViewportOverlay::new(viewport, cube);
        assert_eq!(overlay.children.len(), 2);
        assert_eq!(overlay.placements.len(), 0);
    }

    #[test]
    fn build_viewport_overlay_has_8_ring_and_3_bottom_widgets() {
        let state = fresh_state();
        let mut overlay = build_viewport_overlay(state, make_font());
        overlay.layout(Size::new(800.0, 600.0));
        // Downcast to ViewportOverlay so we can inspect children.
        // `build_viewport_overlay` returns a Box<dyn Widget>; we
        // routed it through our type, so the type_name should match.
        assert_eq!(overlay.type_name(), "ViewportOverlay");
        // 2 (viewport + cube) + 8 ring + 3 bottom = 13 total children.
        assert_eq!(overlay.children().len(), 13);
    }

    #[test]
    fn home_button_resets_camera_orientation() {
        let state = fresh_state();
        // Move the camera off-default first.
        {
            let mut c = state.camera.lock().unwrap();
            c.azimuth = 1.234;
            c.elevation = 0.789;
        }
        let mut overlay = build_at_size(state.clone(), 800.0, 600.0);
        // Find the Home button's bounds.  It's at angle Tau * 0.30,
        // ring radius 60 px from the cube centre. Compute its centre
        // in overlay-local coords (matches the layout in `layout()`).
        let h = 600.0_f64;
        let w = 800.0_f64;
        let cube_cx = w - CUBE_MARGIN_RIGHT - CUBE_SIZE * 0.5;
        let cube_cy = h - CUBE_MARGIN_TOP - CUBE_SIZE * 0.5;
        let angle = TAU * 0.30;
        let dx = -angle.sin() * RING_RADIUS;
        let dy = angle.cos() * RING_RADIUS;
        let center = Point::new(cube_cx + dx, cube_cy + dy);

        // Force a re-layout (some children only update on the first
        // post-construction layout pass).
        overlay.layout(Size::new(w, h));

        // Cast back to ViewportOverlay for the click helper.
        // `Box<dyn Widget>` can be re-borrowed mutably to call
        // on_event without downcasting since the trait is enough.
        overlay.on_event(&Event::MouseDown {
            pos: center,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        });
        overlay.on_event(&Event::MouseUp {
            pos: center,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        });

        let c = state.camera.lock().unwrap();
        let d = atomartist_renderer::OrbitCamera::default();
        // Allow for floating-point fuzz; reset_view sets both to
        // the default's exact values.
        assert!((c.azimuth - d.azimuth).abs() < 1e-3, "az = {}", c.azimuth);
        assert!((c.elevation - d.elevation).abs() < 1e-3, "el = {}", c.elevation);
    }

    #[test]
    fn bed_toggle_flips_show_bed_state() {
        let state = fresh_state();
        let initial = *state.show_bed.lock().unwrap();
        let mut overlay = build_at_size(state.clone(), 800.0, 600.0);
        // Bed button is now the first row of the vertical stack —
        // centred horizontally under the cube, at dy_below =
        // BOTTOM_ROW_TOP_OFFSET - CUBE_MARGIN_TOP - CUBE_SIZE = 35.
        let h = 600.0_f64;
        let w = 800.0_f64;
        let cube_cx = w - CUBE_MARGIN_RIGHT - CUBE_SIZE * 0.5;
        let cube_y = h - CUBE_MARGIN_TOP - CUBE_SIZE; // bottom of cube
        let dy_below = BOTTOM_ROW_TOP_OFFSET - CUBE_MARGIN_TOP - CUBE_SIZE;
        let center = Point::new(cube_cx, cube_y - dy_below);
        overlay.layout(Size::new(w, h));
        click_at_widget(&mut *overlay, center);
        let after = *state.show_bed.lock().unwrap();
        assert_ne!(initial, after, "bed click should flip show_bed");
    }

    /// Variant of `click_at` that works on a `dyn Widget` directly,
    /// for the `Box<dyn Widget>` returned by `build_viewport_overlay`.
    fn click_at_widget(w: &mut dyn Widget, pos: Point) {
        w.on_event(&Event::MouseDown {
            pos,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        });
        w.on_event(&Event::MouseUp {
            pos,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        });
    }

    // Touch the local helper so it's not flagged unused.
    #[test]
    fn click_at_helper_is_callable() {
        let state = fresh_state();
        let viewport_inputs = ViewportInputs::empty();
        let cube_inputs = TumbleCubeInputs { camera: state.camera.clone() };
        let viewport = Box::new(Viewport3dWidget::new(viewport_inputs));
        let cube = Box::new(TumbleCubeWidget::new(cube_inputs));
        let mut overlay = ViewportOverlay::new(viewport, cube);
        overlay.layout(Size::new(800.0, 600.0));
        click_at(&mut overlay, Point::new(10.0, 10.0));
    }
}
