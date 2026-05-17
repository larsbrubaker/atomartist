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
    text::Font, Color, DrawCtx, Event, EventResult, HAnchor, MouseButton, Point, Rect, Size,
    VAnchor, Widget, WidgetBase,
};
use agg_gui::widgets::Button;
use atomartist_renderer::{
    OrbitMode, Projection, RenderStyle, TumbleCubeInputs, TumbleCubeWidget, Viewport3dWidget,
    ViewportInputs, ViewportTool,
};

use crate::app_state::AppState;

/// Pixel side of the tumble cube widget itself (radius from centre is
/// `CUBE_SIZE / 2`).
const CUBE_SIZE: f64 = 100.0;
/// Distance from the cube centre to button centres on the ring.  Match
/// MatterCAD's `radius = 70 * DeviceScale` heuristic.
const RING_RADIUS: f64 = 70.0;
/// Right / top margin of the cube within the overlay.  Matches
/// MatterCAD's `Margin = new BorderDouble(0, 0, 40, 45)`.
const CUBE_MARGIN_RIGHT: f64 = 40.0;
const CUBE_MARGIN_TOP: f64 = 45.0;
/// Side of every ring button — small enough that ten can fit on the
/// rim without overlapping.
const BUTTON_SIZE: f64 = 32.0;
/// Side of each render-style segment under the cube.
const STYLE_BUTTON_W: f64 = 40.0;
const STYLE_BUTTON_H: f64 = 24.0;

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
    add_tool_button(&mut overlay, &state, &font, TAU * 0.15, "Sel", ViewportTool::Select);
    add_tool_button(&mut overlay, &state, &font, TAU * 0.05, "Rot", ViewportTool::Rotate);
    add_tool_button(&mut overlay, &state, &font, -TAU * 0.05, "Pan", ViewportTool::Pan);
    add_tool_button(&mut overlay, &state, &font, -TAU * 0.15, "Zoom", ViewportTool::Zoom);
    add_home_button(&mut overlay, &state, &font, TAU * 0.30);
    add_zoom_to_sel_button(&mut overlay, &state, &font, TAU * 0.40);
    add_turntable_button(&mut overlay, &state, &font, -TAU * 0.40);
    add_perspective_button(&mut overlay, &state, &font, -TAU * 0.30);

    add_render_style_segments(&mut overlay, &state, &font);

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
        // HUD background: faint rounded arcs / line behind the buttons,
        // port of `View3DWidget.cs`'s `renderRoundedGroup` /
        // `renderRoundedLine` inline lambdas (lines 498-552).
        let w = self.bounds.width;
        let h = self.bounds.height;
        let cube_cx = w - CUBE_MARGIN_RIGHT - CUBE_SIZE * 0.5;
        let cube_cy = h - CUBE_MARGIN_TOP - CUBE_SIZE * 0.5;
        let cube_r = CUBE_SIZE * 0.5;
        let ring_r = cube_r + 12.0 + 8.5;

        let visuals = ctx.visuals();
        let dark = 0.299 * visuals.bg_color.r
            + 0.587 * visuals.bg_color.g
            + 0.114 * visuals.bg_color.b
            < 0.5;
        let hud_bg = if dark {
            Color::rgba(0.10, 0.11, 0.13, 0.55)
        } else {
            Color::rgba(0.95, 0.95, 0.97, 0.55)
        };
        let hud_stroke = if dark {
            Color::rgba(0.85, 0.86, 0.90, 0.45)
        } else {
            Color::rgba(0.25, 0.26, 0.30, 0.45)
        };

        let _ = hud_stroke;
        // The three button-group arcs MatterCAD draws.
        paint_arc_segment(ctx, cube_cx, cube_cy, ring_r, TAU * 0.10, TAU * 0.40, 17.0, hud_bg);
        paint_arc_segment(ctx, cube_cx, cube_cy, ring_r, TAU * (0.5 + 0.05), TAU * (0.5 + 0.15), 17.0, hud_bg);
        paint_arc_segment(ctx, cube_cx, cube_cy, ring_r, TAU * (1.0 - 0.15), TAU * (1.0 - 0.05), 17.0, hud_bg);

        // Render-style line below the cube.
        paint_rounded_line(
            ctx,
            cube_cx,
            cube_cy - CUBE_SIZE * 0.5 - 60.0,
            70.0,
            STYLE_BUTTON_H + 6.0,
            hud_bg,
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

/// Filled-stroke arc segment used by the HUD background.  `a_start` and
/// `a_end` are in MatterCAD's convention (0 = up, CCW positive); we
/// convert to atan2's convention (0 = right, CCW positive) inside.
fn paint_arc_segment(
    ctx: &mut dyn DrawCtx,
    cx: f64,
    cy: f64,
    radius: f64,
    a_start: f64,
    a_end: f64,
    width: f64,
    fill: Color,
) {
    let to_atan2 = |a: f64| std::f64::consts::FRAC_PI_2 - a;
    let a0 = to_atan2(a_start);
    let a1 = to_atan2(a_end);
    let (a_lo, a_hi) = if a0 < a1 { (a0, a1) } else { (a1, a0) };
    let steps = 24;
    ctx.set_line_width(width);
    ctx.set_stroke_color(fill);
    ctx.begin_path();
    for i in 0..=steps {
        let t = i as f64 / steps as f64;
        let a = a_lo + (a_hi - a_lo) * t;
        let x = cx + radius * a.cos();
        let y = cy + radius * a.sin();
        if i == 0 {
            ctx.move_to(x, y);
        } else {
            ctx.line_to(x, y);
        }
    }
    ctx.stroke();
}

/// Horizontal rounded line behind the render-style picker.
fn paint_rounded_line(
    ctx: &mut dyn DrawCtx,
    cx: f64,
    cy: f64,
    half_len: f64,
    width: f64,
    fill: Color,
) {
    ctx.set_line_width(width);
    ctx.set_stroke_color(fill);
    ctx.begin_path();
    ctx.move_to(cx - half_len, cy);
    ctx.line_to(cx + half_len, cy);
    ctx.stroke();
}

// ---------------------------------------------------------------------------
// Button helpers
// ---------------------------------------------------------------------------

fn add_tool_button(
    overlay: &mut ViewportOverlay,
    state: &AppState,
    font: &Arc<Font>,
    angle: f64,
    label: &str,
    tool: ViewportTool,
) {
    let tool_w = state.viewport_tool.clone();
    let tool_r = state.viewport_tool.clone();
    let btn = Button::new(label, font.clone())
        .with_font_size(11.0)
        .with_subtle()
        .with_outlined()
        .with_active_fn(move || *tool_r.lock().unwrap() == tool)
        .on_click(move || {
            *tool_w.lock().unwrap() = tool;
        });
    overlay.add_ring_button(Box::new(btn), angle);
}

fn add_home_button(overlay: &mut ViewportOverlay, state: &AppState, font: &Arc<Font>, angle: f64) {
    let camera = state.camera.clone();
    let btn = Button::new("Home", font.clone())
        .with_font_size(11.0)
        .with_subtle()
        .with_outlined()
        .on_click(move || {
            camera.lock().unwrap().reset_view();
        });
    overlay.add_ring_button(Box::new(btn), angle);
}

fn add_zoom_to_sel_button(
    overlay: &mut ViewportOverlay,
    state: &AppState,
    font: &Arc<Font>,
    angle: f64,
) {
    let mesh_slot = state.last_mesh_output.clone();
    let camera = state.camera.clone();
    let btn = Button::new("Fit", font.clone())
        .with_font_size(11.0)
        .with_subtle()
        .with_outlined()
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

fn add_turntable_button(
    overlay: &mut ViewportOverlay,
    state: &AppState,
    font: &Arc<Font>,
    angle: f64,
) {
    let camera_w = state.camera.clone();
    let setting_w = state.turntable.clone();
    let setting_r = state.turntable.clone();
    let btn = Button::new("Turn", font.clone())
        .with_font_size(11.0)
        .with_subtle()
        .with_outlined()
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

fn add_perspective_button(
    overlay: &mut ViewportOverlay,
    state: &AppState,
    font: &Arc<Font>,
    angle: f64,
) {
    let camera_w = state.camera.clone();
    let setting_w = state.perspective.clone();
    let setting_r = state.perspective.clone();
    let btn = Button::new("Persp", font.clone())
        .with_font_size(11.0)
        .with_subtle()
        .with_outlined()
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

fn add_render_style_segments(overlay: &mut ViewportOverlay, state: &AppState, font: &Arc<Font>) {
    let styles: [(&str, RenderStyle); 3] = [
        ("Shade", RenderStyle::Shaded),
        ("Out", RenderStyle::OutlineOnly),
        ("Wire", RenderStyle::Wireframe),
    ];
    for (i, (label, variant)) in styles.iter().enumerate() {
        let style_w = state.render_style.clone();
        let style_r = state.render_style.clone();
        let variant_ = *variant;
        let btn = Button::new(*label, font.clone())
            .with_font_size(10.0)
            .with_subtle()
            .with_outlined()
            .with_active_fn(move || *style_r.lock().unwrap() == variant_)
            .on_click(move || {
                *style_w.lock().unwrap() = variant_;
            });
        let dx = (i as f64 - 1.0) * (STYLE_BUTTON_W + 4.0);
        overlay.add_below_cube(
            Box::new(btn),
            dx,
            60.0,
            Size::new(STYLE_BUTTON_W, STYLE_BUTTON_H),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;

    fn make_font() -> Arc<Font> {
        // Tests don't always need a real font, but build_app expects
        // one. Use whatever the test harness installed via
        // font_settings, falling back to a minimal stub.
        agg_gui::font_settings::current_system_font()
            .unwrap_or_else(|| Arc::new(Font::from_bytes(Vec::new()).unwrap_or_else(|_| panic!("no font"))))
    }

    #[test]
    fn overlay_constructs_with_eight_ring_buttons_and_three_style_segments() {
        let state = AppState::new(
            atomartist_lib::Graph::new(),
            atomartist_lib::registry::NodeRegistry::new(),
        );
        // Without a font, the overlay is still constructible because
        // labels degrade to background-only paints. Build it directly
        // bypassing build_viewport_overlay to avoid needing a font.
        let _ = make_font; // suppress unused warning in this test
        let viewport_inputs = ViewportInputs::empty();
        let cube_inputs = TumbleCubeInputs { camera: state.camera.clone() };
        let viewport = Box::new(Viewport3dWidget::new(viewport_inputs));
        let cube = Box::new(TumbleCubeWidget::new(cube_inputs));
        let overlay = ViewportOverlay::new(viewport, cube);
        // 2 fixed + 0 ring/below at this point.
        assert_eq!(overlay.children.len(), 2);
        assert_eq!(overlay.placements.len(), 0);
    }
}

// Suppress an unused-import warning until the test module references
// `MouseButton` directly.
#[allow(dead_code)]
fn _silence_unused() {
    let _ = MouseButton::Left;
}
