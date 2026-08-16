//! Node-canvas navigation cluster — the home button and the
//! Select / Pan / Zoom mode group that float over the graph pane's
//! top-left corner (design §5d step 6h-3; port of NodeDesigner's
//! `_graph_panel.ejs` `.graph-controls` + `graph-manager.js`).
//!
//! ```text
//!  ┌──────┐   ┌──────┬──────┬──────┐
//!  │ home │   │ sel  │ pan  │ zoom │   ← 12 px in, 12 px down from the top
//!  └──────┘   └──────┴──────┴──────┘
//!  ┌───────────────────────────────────────────────┐
//!  │                node canvas                    │
//! ```
//!
//! The cluster is one widget with four hit rectangles rather than four
//! button widgets: the mode group is a *segmented* control (shared
//! rounded outline, hairline separators, one accent-tinted selection),
//! which is a single drawing, and one widget means one `properties()`
//! row for the tests and one hover/tooltip state machine.
//!
//! It is mounted as the upper child of a `Stack` over the canvas pane
//! (see [`crate::top_level`]), so its bounds are the whole pane; only
//! [`Widget::hit_test`] narrows it to the button rectangles, and
//! everywhere else the press falls through to the editor underneath —
//! the same "invisible until it matters" contract
//! [`crate::floating_overlay::FloatingOverlayHost`] uses.
//!
//! Nothing here mutates the view directly: every action becomes a
//! [`NodeEditorCommand`] on the shared
//! [`AppState::node_editor`](crate::AppState) handle, because pan / zoom
//! live inside the `NodeEditor` widget and no sibling can reach it.
//!
//! **Y-up.** ND's CSS insets are measured from the pane's top edge; here
//! the row's `y` is `height - TOP_INSET - BUTTON`, recomputed every
//! layout so the cluster stays pinned to the top as the splitter moves.

use agg_gui::{
    font_settings, DrawCtx, Event, EventResult, HAnchor, Insets, MouseButton, Point, Rect, Size,
    VAnchor, Widget, WidgetBase,
};
use agg_gui_node_editor::{InteractionMode, NodeEditorCommand, NodeEditorHandle};

/// Left inset of the home button (ND: `left: 12px`).
pub const HOME_X: f64 = 12.0;
/// Distance from the pane's **top** edge to the top of both groups
/// (ND: `top: 12px`).
pub const TOP_INSET: f64 = 12.0;
/// Side of every button in the cluster.
pub const BUTTON: f64 = 40.0;
/// Left edge of the segmented mode group (ND: `left: 64px`).
pub const GROUP_X: f64 = 64.0;
/// Corner radius of both groups.
const RADIUS: f64 = 6.0;
/// Glyph size inside a button.
const GLYPH_SIZE: f64 = 15.0;

/// The three segments, in display order.
const MODES: [InteractionMode; 3] = [
    InteractionMode::Select,
    InteractionMode::Pan,
    InteractionMode::Zoom,
];

/// What the pointer is currently over. Drives both the hover tint and
/// [`Widget::tooltip_text`], which is why it is `Copy` and cheap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Region {
    Home,
    Mode(usize),
}

impl Region {
    /// ND's `title` attributes, verbatim.
    fn tooltip(self) -> &'static str {
        match self {
            Region::Home => "Reset graph view",
            Region::Mode(0) => "Select Mode",
            Region::Mode(1) => "Pan Mode (middle click)",
            _ => "Zoom Mode (scroll wheel)",
        }
    }
}

/// Glyph for one mode segment.
fn mode_glyph(mode: InteractionMode) -> char {
    match mode {
        InteractionMode::Select => crate::fa::MOUSE_POINTER,
        InteractionMode::Pan => crate::fa::ARROWS,
        InteractionMode::Zoom => crate::fa::SEARCH_PLUS,
    }
}

/// Short name used by `properties()` (and therefore by the UI tests).
fn mode_name(mode: InteractionMode) -> &'static str {
    match mode {
        InteractionMode::Select => "select",
        InteractionMode::Pan => "pan",
        InteractionMode::Zoom => "zoom",
    }
}

pub struct GraphNavCluster {
    bounds: Rect,
    children: Vec<Box<dyn Widget>>,
    base: WidgetBase,
    /// Command channel to the `NodeEditor` in the pane below.
    editor: NodeEditorHandle,
    /// The mode this cluster last asked for. The editor holds the
    /// authoritative copy; the cluster is its only writer, so the mirror
    /// cannot drift.
    mode: InteractionMode,
    hovered: Option<Region>,
    /// Region the pointer went down on, so a press that slides off the
    /// button before release does nothing (standard button contract).
    pressed: Option<Region>,
}

impl GraphNavCluster {
    pub fn new(editor: NodeEditorHandle) -> Self {
        Self {
            bounds: Rect::default(),
            children: Vec::new(),
            base: WidgetBase::new()
                .with_h_anchor(HAnchor::STRETCH)
                .with_v_anchor(VAnchor::STRETCH),
            editor,
            mode: InteractionMode::default(),
            hovered: None,
            pressed: None,
        }
    }

    /// Y of the button row's **bottom** edge in Y-up widget-local space.
    fn row_y(&self) -> f64 {
        self.bounds.height - TOP_INSET - BUTTON
    }

    fn home_rect(&self) -> Rect {
        Rect::new(HOME_X, self.row_y(), BUTTON, BUTTON)
    }

    fn mode_rect(&self, index: usize) -> Rect {
        Rect::new(
            GROUP_X + index as f64 * BUTTON,
            self.row_y(),
            BUTTON,
            BUTTON,
        )
    }

    /// Local centre of a button — the coordinate a UI test clicks.
    pub fn home_center(pane_height: f64) -> Point {
        Point::new(
            HOME_X + BUTTON * 0.5,
            pane_height - TOP_INSET - BUTTON * 0.5,
        )
    }

    /// Local centre of mode segment `index` (0 = Select, 1 = Pan, 2 = Zoom).
    pub fn mode_center(pane_height: f64, index: usize) -> Point {
        Point::new(
            GROUP_X + index as f64 * BUTTON + BUTTON * 0.5,
            pane_height - TOP_INSET - BUTTON * 0.5,
        )
    }

    fn region_at(&self, pos: Point) -> Option<Region> {
        if self.home_rect().contains(pos) {
            return Some(Region::Home);
        }
        for i in 0..MODES.len() {
            if self.mode_rect(i).contains(pos) {
                return Some(Region::Mode(i));
            }
        }
        None
    }

    /// Adopt a hover region, redrawing when it actually changes. Hover is
    /// mutated in a `MouseMove` that may return `Ignored`, so the redraw
    /// has to be explicit (`Widget::on_event`'s invalidation contract).
    fn set_hovered(&mut self, region: Option<Region>) {
        if self.hovered == region {
            return;
        }
        self.hovered = region;
        self.base.tooltip = region.map(|r| r.tooltip().to_string());
        agg_gui::animation::request_draw();
    }

    /// Run a button. Home animates a fit-all; a segment re-binds the
    /// left mouse button.
    fn activate(&mut self, region: Region) {
        match region {
            Region::Home => self.editor.push(NodeEditorCommand::FitToContent),
            Region::Mode(index) => {
                let mode = MODES[index];
                if self.mode != mode {
                    self.mode = mode;
                    self.editor
                        .push(NodeEditorCommand::SetInteractionMode(mode));
                }
            }
        }
        agg_gui::animation::request_draw();
    }

    /// Paint one button's background + glyph. `selected` gets the accent
    /// tint, `hovered` the lift (a brighter fill — we have no shadow
    /// system, and the theme's `widget_bg_hovered` is the house idiom).
    fn paint_button(
        &self,
        ctx: &mut dyn DrawCtx,
        rect: Rect,
        glyph: char,
        selected: bool,
        hovered: bool,
        radius: (bool, bool),
    ) {
        let v = ctx.visuals();
        let fill = if selected {
            v.accent
        } else if hovered {
            v.widget_bg_hovered
        } else {
            v.widget_bg
        };
        // Segmented ends: only the group's outer corners round. A
        // segment whose inner side must stay square draws a rounded rect
        // that overhangs on that side and clips it away — overhanging
        // without the clip would let a segment repaint its neighbour's
        // edge in the wrong colour.
        let (round_left, round_right) = radius;
        let x = if round_left { rect.x } else { rect.x - RADIUS };
        let right = if round_right {
            rect.x + rect.width
        } else {
            rect.x + rect.width + RADIUS
        };
        ctx.save();
        ctx.clip_rect(rect.x, rect.y, rect.width, rect.height);
        ctx.set_fill_color(fill);
        ctx.begin_path();
        ctx.rounded_rect(x, rect.y, right - x, rect.height, RADIUS);
        ctx.fill();
        ctx.restore();

        let text = glyph.to_string();
        ctx.set_font_size(GLYPH_SIZE);
        let metrics = ctx.measure_text(&text);
        let gw = metrics.as_ref().map(|m| m.width).unwrap_or(GLYPH_SIZE);
        let ascent = metrics
            .as_ref()
            .map(|m| m.ascent)
            .unwrap_or(GLYPH_SIZE * 0.8);
        let descent = metrics.as_ref().map(|m| m.descent).unwrap_or(0.0);
        ctx.set_fill_color(if selected {
            v.window_title_text
        } else {
            v.text_color
        });
        ctx.fill_text(
            &text,
            rect.x + (rect.width - gw) * 0.5,
            rect.y + (rect.height - ascent - descent) * 0.5,
        );
    }
}

impl Widget for GraphNavCluster {
    fn type_name(&self) -> &'static str {
        "GraphNavCluster"
    }
    fn id(&self) -> Option<&str> {
        Some("graph-nav")
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
    fn margin(&self) -> Insets {
        self.base.margin
    }
    fn widget_base(&self) -> Option<&WidgetBase> {
        Some(&self.base)
    }

    fn layout(&mut self, available: Size) -> Size {
        self.bounds = Rect::new(0.0, 0.0, available.width, available.height);
        available
    }

    /// Only the buttons are solid: everywhere else the canvas below owns
    /// the pointer.
    fn hit_test(&self, local_pos: Point) -> bool {
        self.region_at(local_pos).is_some()
    }

    fn paint(&mut self, ctx: &mut dyn DrawCtx) {
        if self.bounds.width <= 0.0 || self.bounds.height <= 0.0 {
            return;
        }
        if let Some(font) = font_settings::current_system_font() {
            ctx.set_font(font);
        }
        let home = self.home_rect();
        self.paint_button(
            ctx,
            home,
            crate::fa::HOME,
            false,
            self.hovered == Some(Region::Home),
            (true, true),
        );
        let last = MODES.len() - 1;
        for (i, mode) in MODES.iter().enumerate() {
            let rect = self.mode_rect(i);
            self.paint_button(
                ctx,
                rect,
                mode_glyph(*mode),
                self.mode == *mode,
                self.hovered == Some(Region::Mode(i)),
                (i == 0, i == last),
            );
            // Hairline between segments — drawn on the leading edge of
            // every segment but the first, so the group reads as one
            // control rather than three buttons.
            if i > 0 {
                let v = ctx.visuals();
                ctx.set_fill_color(v.separator);
                ctx.begin_path();
                ctx.rect(rect.x, rect.y + 6.0, 1.0, rect.height - 12.0);
                ctx.fill();
            }
        }
    }

    fn on_event(&mut self, event: &Event) -> EventResult {
        match event {
            Event::MouseMove { pos } => {
                self.set_hovered(self.region_at(*pos));
                EventResult::Ignored
            }
            Event::MouseDown {
                pos,
                button: MouseButton::Left,
                ..
            } => match self.region_at(*pos) {
                Some(region) => {
                    self.pressed = Some(region);
                    EventResult::Consumed
                }
                None => EventResult::Ignored,
            },
            Event::MouseUp {
                pos,
                button: MouseButton::Left,
                ..
            } => {
                let pressed = self.pressed.take();
                match (pressed, self.region_at(*pos)) {
                    // Press and release on the same button = a click.
                    (Some(a), Some(b)) if a == b => {
                        self.activate(a);
                        EventResult::Consumed
                    }
                    (Some(_), _) => EventResult::Consumed,
                    _ => EventResult::Ignored,
                }
            }
            _ => EventResult::Ignored,
        }
    }

    /// Inspector / UI-test surface: the active mode and what the pointer
    /// is over.
    fn properties(&self) -> Vec<(&'static str, String)> {
        vec![
            ("mode", mode_name(self.mode).to_string()),
            (
                "hovered",
                match self.hovered {
                    None => "none".to_string(),
                    Some(Region::Home) => "home".to_string(),
                    Some(Region::Mode(i)) => mode_name(MODES[i]).to_string(),
                },
            ),
        ]
    }
}

#[cfg(test)]
#[path = "graph_nav_tests.rs"]
mod graph_nav_tests;
