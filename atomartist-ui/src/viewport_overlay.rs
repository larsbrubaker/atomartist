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
    text::Font, DrawCtx, Event, EventResult, HAnchor, Point, Rect, Size, Tooltip, VAnchor,
    Widget, WidgetBase,
};
use atomartist_renderer::{
    CameraPoseAnimation, OrbitCamera, OrbitMode, Projection, ProjectionAnimation, RenderStyle,
    TumbleCubeInputs, TumbleCubeWidget, Viewport3dWidget, ViewportInputs, ViewportTool,
};

use crate::app_state::AppState;
use crate::circular_dropdown::{CircularDropdown, DropdownItem};
use crate::circular_icon_button::CircularIconButton;
use crate::icons::IconKind;
use crate::mattercad_icons::MatterCadIcon;

#[path = "viewport_overlay_hud_bay.rs"]
mod viewport_overlay_hud_bay;
use viewport_overlay_hud_bay::HudBayLayer;

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
        camera_animation: state.camera_animation.clone(),
        projection_animation: state.projection_animation.clone(),
    };
    let cube_inputs = TumbleCubeInputs {
        camera: state.camera.clone(),
        animation_completed: None,
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
    // Tool radio cluster — each button maps to one of MatterCAD's
    // `ThemedRadioIconButton` entries in `View3DWidget.CreateTumbleCubeAndControls`.
    add_tool_button(
        &mut overlay, &state, &font, TAU * 0.15,
        MatterCadIcon::PartSelect, ViewportTool::Select,
        "Select parts",
    );
    add_tool_button(
        &mut overlay, &state, &font, TAU * 0.05,
        MatterCadIcon::Rotate, ViewportTool::Rotate,
        "Rotate view",
    );
    add_tool_button(
        &mut overlay, &state, &font, -TAU * 0.05,
        MatterCadIcon::Translate, ViewportTool::Pan,
        "Move view",
    );
    add_tool_button(
        &mut overlay, &state, &font, -TAU * 0.15,
        MatterCadIcon::Scale, ViewportTool::Zoom,
        "Zoom view",
    );
    add_home_button(&mut overlay, &state, &font, TAU * 0.30);
    add_zoom_to_sel_button(&mut overlay, &state, &font, TAU * 0.40);
    add_turntable_button(&mut overlay, &state, &font, -TAU * 0.40);
    add_perspective_button(&mut overlay, &state, &font, -TAU * 0.30);

    add_bottom_row(&mut overlay, &state, &font);

    Box::new(overlay)
}

/// Placement rule for an overlay child past the viewport + cube +
/// HUD-bay layer.
#[derive(Clone, Copy, Debug)]
enum Placement {
    /// Centre on the ring at the given polar angle (radians, 0 = up).
    Ring { angle: f64, size: Size },
    /// Centre at `(cube_cx + dx, cube_cy_top - dy)` — used by the
    /// render-style picker beneath the cube.
    BelowCube { dx: f64, dy_below: f64, size: Size },
}

/// Z-order positions in the overlay's child list.  All children
/// after `HUD_BAY` are buttons / dropdowns laid out via
/// `Placement`.
const CHILD_VIEWPORT: usize = 0;
const CHILD_HUD_BAY: usize = 1;
const CHILD_CUBE: usize = 2;
const FIRST_BUTTON_CHILD: usize = 3;

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
        let bay: Box<dyn Widget> = Box::new(HudBayLayer::new());
        Self {
            bounds: Rect::default(),
            children: vec![viewport, bay, cube],
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

        // children[CHILD_VIEWPORT]: viewport — stretches to fill.
        if let Some(vp) = self.children.get_mut(CHILD_VIEWPORT) {
            vp.layout(available);
            vp.set_bounds(Rect::new(0.0, 0.0, available.width, available.height));
        }
        // children[CHILD_HUD_BAY]: HUD bay framing — stretches to
        // fill so it can paint the banana arcs OVER the viewport's
        // own background fill but BELOW the buttons.
        if let Some(bay) = self.children.get_mut(CHILD_HUD_BAY) {
            bay.layout(available);
            bay.set_bounds(Rect::new(0.0, 0.0, available.width, available.height));
        }
        // children[CHILD_CUBE]: tumble cube.
        if let Some(cube) = self.children.get_mut(CHILD_CUBE) {
            cube.layout(Size::new(cube_w, cube_h));
            cube.set_bounds(Rect::new(cube_x, cube_y, cube_w, cube_h));
        }

        for (i, placement) in self.placements.iter().enumerate() {
            let Some(child) = self.children.get_mut(FIRST_BUTTON_CHILD + i) else { continue };
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

    fn paint(&mut self, _ctx: &mut dyn DrawCtx) {
        // The HUD framing arcs used to live here, but the viewport
        // widget (child 0) paints its own background fill in `paint`
        // which would overwrite anything drawn by the overlay
        // itself. Bays are now painted by `HudBayLayer` (child 1)
        // which runs AFTER the viewport, AFTER its bg fill, but
        // BEFORE the buttons (children 3..).
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

// HudBayLayer + its AGG arc/circle paint helpers live in the
// sibling `viewport_overlay_hud_bay.rs` so this file stays under
// the repository line-count guardrail.

// ---------------------------------------------------------------------------
// Button helpers — every ring button is a `CircularIconButton` so the
// HUD matches MatterCAD's circular `ThemedRadioIconButton` aesthetic
// instead of agg-gui's rectangular `Button` chrome.
// ---------------------------------------------------------------------------

fn add_tool_button(
    overlay: &mut ViewportOverlay,
    state: &AppState,
    font: &Arc<Font>,
    angle: f64,
    image: MatterCadIcon,
    tool: ViewportTool,
    tooltip: &'static str,
) {
    let tool_w = state.viewport_tool.clone();
    let tool_r = state.viewport_tool.clone();
    let btn = CircularIconButton::new(IconKind::Select)
        .with_image_icon(image)
        .with_active_fn(move || *tool_r.lock().unwrap() == tool)
        .on_click(move || {
            *tool_w.lock().unwrap() = tool;
        });
    overlay.add_ring_button(wrap_tooltip(Box::new(btn), tooltip, font), angle);
}

fn add_home_button(overlay: &mut ViewportOverlay, state: &AppState, font: &Arc<Font>, angle: f64) {
    let camera = state.camera.clone();
    let animation = state.camera_animation.clone();
    let btn = CircularIconButton::new(IconKind::Home)
        .with_image_icon(MatterCadIcon::Home)
        .on_click(move || {
            let start = camera.lock().unwrap().clone();
            let mut target = start.clone();
            target.reset_view();
            *animation.lock().unwrap() = Some(CameraPoseAnimation::new(&start, target, 0.25));
            agg_gui::animation::request_draw();
        });
    overlay.add_ring_button(wrap_tooltip(Box::new(btn), "Reset view", font), angle);
}

fn add_zoom_to_sel_button(overlay: &mut ViewportOverlay, state: &AppState, font: &Arc<Font>, angle: f64) {
    let mesh_slot = state.last_mesh_output.clone();
    let camera = state.camera.clone();
    let animation = state.camera_animation.clone();
    let btn = CircularIconButton::new(IconKind::Fit)
        .with_image_icon(MatterCadIcon::Select)
        .on_click(move || {
            let Some(geom) = mesh_slot.lock().unwrap().clone() else { return };
            let mesh = &geom.mesh;
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
                let start = camera.lock().unwrap().clone();
                let mut target = start.clone();
                target.fit_to_bounds(mn, mx);
                *animation.lock().unwrap() = Some(CameraPoseAnimation::new(&start, target, 0.25));
                agg_gui::animation::request_draw();
            }
        });
    overlay.add_ring_button(wrap_tooltip(Box::new(btn), "Zoom to selection", font), angle);
}

/// Turntable / Trackball orbit-mode toggle.
///
/// Toggling INTO turntable kicks off a 0.25-second
/// `CameraPoseAnimation` (slerped via `Quat::slerp`) that animates
/// the camera onto the minimum-angle "level horizon" target
/// produced by `OrbitCamera::snap_to_turntable_alignment`. The
/// snap itself uses `Quat::from_rotation_arc` so the camera moves
/// the **smallest** possible amount — animated, so the user can
/// see and follow the re-leveling instead of being teleported.
/// If the camera is already upright (`snap_to_turntable_alignment`
/// is a no-op) no animation runs.
///
/// MatterCAD's `AddTurnTableButton` flags its equivalent path as
/// "WIP, this should fix the current rotation rather than reset
/// the view" (`View3DWidget.cs:741-744`); this animated minimum-
/// rotation is precisely what that comment was asking for.
fn add_turntable_button(overlay: &mut ViewportOverlay, state: &AppState, font: &Arc<Font>, angle: f64) {
    let camera_w = state.camera.clone();
    let animation_w = state.camera_animation.clone();
    let setting_w = state.turntable.clone();
    let setting_r = state.turntable.clone();
    let btn = CircularIconButton::new(IconKind::Turn)
        .with_image_icon(MatterCadIcon::Spin)
        .with_active_fn(move || *setting_r.lock().unwrap())
        .on_click(move || {
            let mut s = setting_w.lock().unwrap();
            *s = !*s;
            // Set the mode regardless of direction — yaw axis
            // changes immediately so the next drag is interpreted
            // under the new mode even before the level-up animation
            // (if any) completes.
            let start_camera: OrbitCamera = {
                let mut c = camera_w.lock().unwrap();
                c.orbit_mode = if *s {
                    OrbitMode::Turntable
                } else {
                    OrbitMode::Trackball
                };
                c.clone()
            };
            if !*s {
                return;
            }
            // Compute the target by running the snap on a clone —
            // we don't want to apply it to the live camera here,
            // because the animation will interpolate from
            // `start_camera` to `target_camera` over 0.25 s.
            let mut target_camera = start_camera.clone();
            target_camera.snap_to_turntable_alignment();
            // Skip the animation if the snap is already a no-op
            // (camera was already upright).
            let cosine = start_camera
                .orientation
                .dot(target_camera.orientation)
                .abs();
            if cosine > 0.9999 {
                return;
            }
            *animation_w.lock().unwrap() =
                Some(CameraPoseAnimation::new(&start_camera, target_camera, 0.25));
            agg_gui::animation::request_draw();
        });
    overlay.add_ring_button(wrap_tooltip(Box::new(btn), "Turntable mode", font), angle);
}

/// Perspective / Orthographic toggle. Instead of snapping the
/// projection mode on click, this kicks off a `ProjectionAnimation`
/// that smoothly tweens FOV and eye distance over ~0.25 s — port of
/// MatterCAD's `TrackballTumbleWidgetExtended.DoSwitchToProjectionMode`.
/// The active-state predicate reads the *intent* flag
/// (`state.perspective`) so the button's checked treatment flips
/// immediately on click, even though the camera's `projection`
/// field only lands at the end of the tween.
fn add_perspective_button(overlay: &mut ViewportOverlay, state: &AppState, font: &Arc<Font>, angle: f64) {
    let camera_w = state.camera.clone();
    let projection_anim = state.projection_animation.clone();
    let setting_w = state.perspective.clone();
    let setting_r = state.perspective.clone();
    let btn = CircularIconButton::new(IconKind::Persp)
        .with_image_icon(MatterCadIcon::Perspective)
        .with_active_fn(move || *setting_r.lock().unwrap())
        .on_click(move || {
            let mut s = setting_w.lock().unwrap();
            *s = !*s;
            let target = if *s {
                Projection::Perspective
            } else {
                Projection::Orthographic
            };
            let camera_snapshot = camera_w.lock().unwrap().clone();
            // Replace any in-flight tween so a rapid double-toggle
            // ends up at the latest target rather than queueing.
            *projection_anim.lock().unwrap() =
                Some(ProjectionAnimation::new(&camera_snapshot, target, 0.25));
            agg_gui::animation::request_draw();
        });
    overlay.add_ring_button(wrap_tooltip(Box::new(btn), "Perspective mode", font), angle);
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
            .with_image_icon(MatterCadIcon::Bed)
            .with_active_fn(move || *bed_r.lock().unwrap())
            .on_click(move || {
                let mut b = bed_w.lock().unwrap();
                *b = !*b;
            });
        overlay.add_below_cube(
            wrap_tooltip(Box::new(btn), "Show / hide bed grid", font),
            0.0,
            base_below,
            Size::new(BUTTON_SIZE, BUTTON_SIZE),
        );
    }

    // Row 2 — Render-mode (Shader) dropdown. The trigger bubble
    // shows the icon for the currently-selected render style,
    // mirroring MatterCAD's `ViewStyleButton` which swaps its icon
    // between view_shaded.png / view_polygons.png when the user
    // picks a row.
    {
        let items = vec![
            DropdownItem { label: "Shaded".into(), value: RenderStyle::Shaded },
            DropdownItem { label: "Wireframe".into(), value: RenderStyle::Wireframe },
        ];
        let drop = CircularDropdown::new_with_image(
            IconKind::Shade,
            Some(MatterCadIcon::ViewShaded),
            items,
            state.render_style.clone(),
            font.clone(),
        )
        .with_value_to_icon(|style: &RenderStyle| {
            Some(match style {
                RenderStyle::Shaded => MatterCadIcon::ViewShaded,
                RenderStyle::Wireframe => MatterCadIcon::ViewPolygons,
            })
        });
        overlay.add_below_cube(
            wrap_tooltip(Box::new(drop), "Render mode", font),
            0.0,
            base_below + BOTTOM_ROW_SPACING,
            Size::new(BUTTON_SIZE, BUTTON_SIZE),
        );
    }

    // Row 3 — Snap-amount dropdown. Values come from MatterCAD's
    // `GridOptionsPanel`: Off plus the explicit 0.1 / 0.25 / 0.5 /
    // 1 / 2 / 5 mm grid steps. The trigger bubble shows the
    // current snap distance as a number ("-" for off) instead of
    // a glyph, matching `GridOptionsPanel.textButton`.
    {
        let items = vec![
            DropdownItem { label: "Off".into(), value: 0.0_f64 },
            DropdownItem { label: "0.1".into(), value: 0.1 },
            DropdownItem { label: "0.25".into(), value: 0.25 },
            DropdownItem { label: "0.5".into(), value: 0.5 },
            DropdownItem { label: "1".into(), value: 1.0 },
            DropdownItem { label: "2".into(), value: 2.0 },
            DropdownItem { label: "5".into(), value: 5.0 },
        ];
        let drop = CircularDropdown::new(
            IconKind::Snap,
            items,
            state.snap_amount.clone(),
            font.clone(),
        )
        .with_value_to_label(|v: &f64| {
            if *v == 0.0 {
                "-".to_string()
            } else if v.fract() == 0.0 {
                format!("{}", *v as i64)
            } else {
                // Strip trailing zeroes so 0.5 stays "0.5", not "0.50".
                let s = format!("{v}");
                s
            }
        });
        overlay.add_below_cube(
            wrap_tooltip(Box::new(drop), "Snap amount", font),
            0.0,
            base_below + BOTTOM_ROW_SPACING * 2.0,
            Size::new(BUTTON_SIZE, BUTTON_SIZE),
        );
    }
}

fn wrap_tooltip(child: Box<dyn Widget>, text: &'static str, font: &Arc<Font>) -> Box<dyn Widget> {
    // Use agg-gui's default widget-anchored placement. Tooltips
    // should appear below the hovered HUD item, matching MatterCAD;
    // `.at_pointer()` would override that and keep the panel attached
    // to the cursor.
    Box::new(Tooltip::new(child, text, font.clone()))
}

#[cfg(test)]
#[path = "viewport_overlay_tests.rs"]
mod viewport_overlay_tests;
