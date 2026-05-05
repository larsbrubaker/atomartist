//! Node graph canvas — a custom `Widget` that renders the entire graph as
//! draggable boxes with socket circles and bezier connections, on an
//! infinite pan/zoom canvas.
//!
//! Pan / zoom state lives directly on the widget (`canvas_offset`,
//! `canvas_scale`). All hit-testing converts mouse positions from
//! widget-local coords (Y-up, origin at bottom-left of the widget) to
//! canvas-space using the inverse of the same transform applied during
//! paint.
//!
//! Interaction is a small state machine — see `CanvasState`. Drawing is
//! delegated to `canvas_draw` to keep this file under the 800-line limit.

use std::collections::HashSet;

use agg_gui::{
    Color, DrawCtx, Event, EventResult, HAnchor, Key, MenuEntry, MenuItem, MenuResponse,
    Modifiers, MouseButton, Point, PopupMenu, Rect, Size, VAnchor, Widget, WidgetBase,
};

use atomartist_lib::graph::graph::{Edge, GraphError};
use atomartist_lib::graph::node::{NodeId, SocketId};
use atomartist_lib::registry::NodeRegistry;
use atomartist_lib::Graph;

use crate::app_state::AppState;
use crate::canvas_draw::{
    draw_bezier_connection, draw_canvas_grid, draw_node, layout_node, CanvasPalette,
    NodeLayoutInfo, PropLayout, SocketLayout, SocketSide,
};

use atomartist_lib::graph::node::PortValue;

const ZOOM_MIN: f64 = 0.15;
const ZOOM_MAX: f64 = 3.0;
const ZOOM_STEP: f64 = 1.1;

/// Interaction state machine. Only one drag at a time.
#[derive(Clone, Debug)]
enum CanvasState {
    Idle,
    PanningCanvas {
        start_offset: [f64; 2],
        start_local: Point,
    },
    DraggingNode {
        ids: Vec<NodeId>,
        /// Per-node start position, captured at mousedown.
        start_positions: Vec<[f64; 2]>,
        start_canvas: [f64; 2],
    },
    DrawingConnection {
        from: SocketId,
        from_canvas: [f64; 2],
        cursor_canvas: [f64; 2],
        from_socket_type: atomartist_lib::SocketType,
    },
    /// Click-and-horizontal-drag editing of a numeric property.
    DraggingProperty {
        node_id: NodeId,
        prop_name: &'static str,
        start_value: f64,
        start_local_x: f64,
        min: Option<f64>,
        max: Option<f64>,
    },
}

/// Custom widget rendering the full graph as a pan/zoom canvas.
pub struct NodeCanvas {
    bounds: Rect,
    children: Vec<Box<dyn Widget>>, // unused — kept for the Widget trait
    base: WidgetBase,
    state: AppState,
    canvas_offset: [f64; 2],
    canvas_scale: f64,
    selected: HashSet<NodeId>,
    palette: CanvasPalette,
    interaction: CanvasState,
    /// Spacebar pan modifier — when held, mouse-left drag pans the canvas
    /// instead of selecting / dragging nodes.
    space_held: bool,
    /// Right-click add-node popup menu, plus the canvas-space position
    /// where the user clicked (used as the new node's position).
    popup: PopupMenu,
    popup_canvas_pos: [f64; 2],
}

impl NodeCanvas {
    pub fn new(state: AppState) -> Self {
        let popup_items = build_add_node_popup_items(&state);
        Self {
            bounds: Rect::new(0.0, 0.0, 0.0, 0.0),
            children: Vec::new(),
            base: WidgetBase::new()
                .with_h_anchor(HAnchor::STRETCH)
                .with_v_anchor(VAnchor::STRETCH),
            state,
            canvas_offset: [0.0, 0.0],
            canvas_scale: 1.0,
            selected: HashSet::new(),
            palette: CanvasPalette::dark(),
            interaction: CanvasState::Idle,
            space_held: false,
            popup: PopupMenu::new(popup_items),
            popup_canvas_pos: [0.0, 0.0],
        }
    }

    pub fn pan(&self) -> [f64; 2] {
        self.canvas_offset
    }

    pub fn scale(&self) -> f64 {
        self.canvas_scale
    }

    pub fn selected_ids(&self) -> &HashSet<NodeId> {
        &self.selected
    }

    fn local_to_canvas(&self, p: Point) -> [f64; 2] {
        [
            (p.x - self.canvas_offset[0]) / self.canvas_scale,
            (p.y - self.canvas_offset[1]) / self.canvas_scale,
        ]
    }

    /// Compute layouts for every node in the current graph. Returned in a
    /// deterministic order (by node id) so paint order is stable; selected
    /// nodes paint last (on top).
    fn snapshot_layouts(&self) -> Vec<NodeLayoutInfo> {
        let g = self.state.graph.lock().unwrap();
        let registry = &self.state.registry;
        let mut layouts: Vec<NodeLayoutInfo> = g
            .nodes()
            .filter_map(|n| registry.get(n.type_id).map(|def| layout_node(n, def.as_ref())))
            .collect();
        layouts.sort_by_key(|l| {
            // Selected nodes last → they paint on top.
            (self.selected.contains(&l.node_id) as u8, l.node_id.0)
        });
        layouts
    }

    fn hit_node(&self, layouts: &[NodeLayoutInfo], canvas_pos: [f64; 2]) -> Option<NodeId> {
        // Iterate top-to-bottom in paint order so the topmost is hit first.
        for l in layouts.iter().rev() {
            if l.body_contains(canvas_pos) {
                return Some(l.node_id);
            }
        }
        None
    }

    fn hit_socket(&self, layouts: &[NodeLayoutInfo], canvas_pos: [f64; 2]) -> Option<(NodeId, SocketLayout)> {
        for l in layouts.iter().rev() {
            if let Some(s) = l.socket_at(canvas_pos) {
                return Some((l.node_id, s.clone()));
            }
        }
        None
    }

    fn hit_property(&self, layouts: &[NodeLayoutInfo], canvas_pos: [f64; 2]) -> Option<(NodeId, PropLayout)> {
        for l in layouts.iter().rev() {
            if let Some(p) = l.prop_at(canvas_pos) {
                return Some((l.node_id, p.clone()));
            }
        }
        None
    }

    /// Action callback for the right-click popup — currently only handles
    /// `"add.{type_id}"` entries. The new node is positioned at the
    /// canvas-space click location captured when the popup opened.
    fn handle_popup_action(&mut self, action: &str) {
        if let Some(type_id) = action.strip_prefix("add.") {
            let interned = self
                .state
                .registry
                .iter()
                .map(|d| d.type_id())
                .find(|s| *s == type_id);
            if let Some(static_id) = interned {
                let pos = self.popup_canvas_pos;
                let mut g = self.state.graph.lock().unwrap();
                let _ = add_node_with_defaults(&mut g, &self.state.registry, static_id, pos);
                drop(g);
                self.state.schedule_evaluate();
            }
        }
    }

    /// If `node_id` has a Geometry3d output, set it as the viewport's
    /// display node so the next evaluate populates `last_mesh_output`.
    fn update_display_node_for(&self, node_id: NodeId) {
        let g = self.state.graph.lock().unwrap();
        let has_geom = g
            .get(node_id)
            .and_then(|n| self.state.registry.get(n.type_id))
            .map(|def| {
                def.output_sockets()
                    .iter()
                    .any(|s| s.socket_type == atomartist_lib::SocketType::Geometry3d)
            })
            .unwrap_or(false);
        drop(g);
        if has_geom {
            self.state.set_display_node(Some(node_id));
        }
    }

    fn begin_drag_node(&mut self, id: NodeId, canvas_start: [f64; 2]) {
        // Build the drag set — the clicked node + any other selected nodes.
        let mut drag_ids: Vec<NodeId> = self.selected.iter().copied().collect();
        if !drag_ids.contains(&id) {
            drag_ids.clear();
            drag_ids.push(id);
            self.selected.clear();
            self.selected.insert(id);
        }
        let g = self.state.graph.lock().unwrap();
        let mut start_positions = Vec::with_capacity(drag_ids.len());
        for &nid in &drag_ids {
            if let Some(n) = g.get(nid) {
                start_positions.push(n.position);
            } else {
                start_positions.push([0.0, 0.0]);
            }
        }
        self.interaction = CanvasState::DraggingNode {
            ids: drag_ids,
            start_positions,
            start_canvas: canvas_start,
        };
    }

    /// Walk node + registry to classify a socket as input or output.
    fn classify_socket(&self, sid: &SocketId) -> Option<SocketSide> {
        let g = self.state.graph.lock().unwrap();
        let node = g.get(sid.node)?;
        let def = self.state.registry.get(node.type_id)?;
        if def.input_sockets().iter().any(|s| s.name == sid.name) {
            Some(SocketSide::Input)
        } else if def.output_sockets().iter().any(|s| s.name == sid.name) {
            Some(SocketSide::Output)
        } else {
            None
        }
    }
}

impl Widget for NodeCanvas {
    fn bounds(&self) -> Rect {
        self.bounds
    }
    fn set_bounds(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }
    fn children(&self) -> &[Box<dyn Widget>] {
        &self.children
    }
    fn children_mut(&mut self) -> &mut Vec<Box<dyn Widget>> {
        &mut self.children
    }

    fn type_name(&self) -> &'static str { "NodeCanvas" }

    fn h_anchor(&self) -> HAnchor { self.base.h_anchor }
    fn v_anchor(&self) -> VAnchor { self.base.v_anchor }
    fn widget_base(&self) -> Option<&WidgetBase> { Some(&self.base) }

    fn layout(&mut self, available: Size) -> Size {
        // Canvas fills its allotted space — anchored.
        self.bounds = Rect::new(0.0, 0.0, available.width, available.height);
        available
    }

    fn paint(&mut self, ctx: &mut dyn DrawCtx) {
        let w = self.bounds.width;
        let h = self.bounds.height;
        if w <= 0.0 || h <= 0.0 {
            return;
        }

        // Pull palette from current visuals each frame so theme switches
        // (light <-> dark) take effect immediately.
        let visuals = ctx.visuals();
        self.palette = CanvasPalette::from_visuals(&visuals);

        // Make text rendering work — agg-gui's DrawCtx requires a font to
        // be installed before fill_text. Pull from the thread-local
        // system font slot.
        if let Some(f) = agg_gui::font_settings::current_system_font() {
            ctx.set_font(f);
        }

        // Outer save: the clip and any subsequent transforms restore back
        // to whatever the parent gave us. Without this, nodes drawn at
        // canvas-y > self.bounds.height bleed into the sibling pane above
        // when the splitter shrinks the canvas.
        ctx.save();
        ctx.clip_rect(0.0, 0.0, w, h);

        // Background
        ctx.set_fill_color(self.palette.canvas_bg);
        ctx.begin_path();
        ctx.rect(0.0, 0.0, w, h);
        ctx.fill();

        // Apply pan + zoom for canvas-space drawing.
        ctx.save();
        ctx.translate(self.canvas_offset[0], self.canvas_offset[1]);
        ctx.scale(self.canvas_scale, self.canvas_scale);

        // Visible canvas-space rect (inverse-transformed widget rect).
        let inv_scale = 1.0 / self.canvas_scale;
        let visible_min = [
            (0.0 - self.canvas_offset[0]) * inv_scale,
            (0.0 - self.canvas_offset[1]) * inv_scale,
        ];
        let visible_max = [
            (w - self.canvas_offset[0]) * inv_scale,
            (h - self.canvas_offset[1]) * inv_scale,
        ];

        draw_canvas_grid(ctx, (visible_min, visible_max), 40.0, self.palette.canvas_grid);

        let layouts = self.snapshot_layouts();

        // Edges first so they pass under nodes.
        let g = self.state.graph.lock().unwrap();
        for edge in g.edges() {
            let from = layouts
                .iter()
                .find(|l| l.node_id == edge.from.node)
                .and_then(|l| l.sockets.iter().find(|s| s.name == edge.from.name));
            let to = layouts
                .iter()
                .find(|l| l.node_id == edge.to.node)
                .and_then(|l| l.sockets.iter().find(|s| s.name == edge.to.name));
            if let (Some(f), Some(t)) = (from, to) {
                let [r, g_, b, a] = f.socket_type.display_color_rgba();
                let col = Color::rgba(
                    r as f32 / 255.0,
                    g_ as f32 / 255.0,
                    b as f32 / 255.0,
                    a as f32 / 255.0,
                );
                draw_bezier_connection(ctx, f.center, t.center, col, 2.0);
            }
        }
        drop(g);

        // Live in-progress connection (during DrawingConnection).
        if let CanvasState::DrawingConnection {
            from_canvas,
            cursor_canvas,
            from_socket_type,
            ..
        } = &self.interaction
        {
            let [r, g_, b, _] = from_socket_type.display_color_rgba();
            let col = Color::rgba(
                r as f32 / 255.0,
                g_ as f32 / 255.0,
                b as f32 / 255.0,
                0.85,
            );
            draw_bezier_connection(ctx, *from_canvas, *cursor_canvas, col, 2.0);
        }

        // Nodes on top.
        for l in &layouts {
            let selected = self.selected.contains(&l.node_id);
            draw_node(ctx, l, selected, &self.palette);
        }

        ctx.restore();   // pop pan/zoom transform

        // Right-click popup paints last so it sits above nodes & connections.
        // Painted INSIDE the clip so the menu also can't escape canvas bounds.
        if self.popup.is_open() {
            if let Some(font) = agg_gui::font_settings::current_system_font() {
                let viewport = Size::new(self.bounds.width, self.bounds.height);
                self.popup.paint(ctx, font, 13.0, viewport);
            }
        }

        ctx.restore();   // pop clip rect
    }

    fn hit_test(&self, local_pos: Point) -> bool {
        local_pos.x >= 0.0
            && local_pos.x <= self.bounds.width
            && local_pos.y >= 0.0
            && local_pos.y <= self.bounds.height
    }

    fn claims_pointer_exclusively(&self, _local_pos: Point) -> bool {
        !matches!(self.interaction, CanvasState::Idle)
    }

    fn on_event(&mut self, event: &Event) -> EventResult {
        // Popup gets first crack when open; on Action, add the chosen node
        // and close. Anything not consumed by the popup falls through to
        // the canvas's normal handling below.
        if self.popup.is_open() {
            let viewport = Size::new(self.bounds.width, self.bounds.height);
            let (result, response) = self.popup.handle_event(event, viewport);
            if let MenuResponse::Action(action) = response {
                self.handle_popup_action(&action);
                self.popup.close();
            } else if let MenuResponse::Closed = response {
                self.popup.close();
            }
            if result == EventResult::Consumed {
                return EventResult::Consumed;
            }
        }
        match event {
            Event::MouseDown { pos, button, modifiers } => self.on_mouse_down(*pos, *button, *modifiers),
            Event::MouseUp { pos, button, modifiers } => self.on_mouse_up(*pos, *button, *modifiers),
            Event::MouseMove { pos } => self.on_mouse_move(*pos),
            Event::MouseWheel { pos, delta_y, modifiers, .. } => {
                self.on_wheel(*pos, *delta_y, *modifiers)
            }
            Event::KeyDown { key, modifiers } => self.on_key_down(key, *modifiers),
            Event::KeyUp { key, modifiers } => self.on_key_up(key, *modifiers),
            _ => EventResult::Ignored,
        }
    }
}

impl NodeCanvas {
    fn on_mouse_down(&mut self, pos: Point, button: MouseButton, modifiers: Modifiers) -> EventResult {
        let canvas_pos = self.local_to_canvas(pos);
        let layouts = self.snapshot_layouts();

        match button {
            MouseButton::Left => {
                if self.space_held {
                    self.interaction = CanvasState::PanningCanvas {
                        start_offset: self.canvas_offset,
                        start_local: pos,
                    };
                    return EventResult::Consumed;
                }
                if let Some((node_id, socket)) = self.hit_socket(&layouts, canvas_pos) {
                    let sid = SocketId { node: node_id, name: socket.name };
                    self.interaction = CanvasState::DrawingConnection {
                        from: sid,
                        from_canvas: socket.center,
                        cursor_canvas: canvas_pos,
                        from_socket_type: socket.socket_type,
                    };
                    return EventResult::Consumed;
                }
                // Property row hit (numeric drag)?
                if let Some((node_id, prop)) = self.hit_property(&layouts, canvas_pos) {
                    if let PortValue::Number(start) = prop.current {
                        self.selected.clear();
                        self.selected.insert(node_id);
                        self.update_display_node_for(node_id);
                        self.interaction = CanvasState::DraggingProperty {
                            node_id,
                            prop_name: prop.name,
                            start_value: start,
                            start_local_x: pos.x,
                            min: prop.min,
                            max: prop.max,
                        };
                        return EventResult::Consumed;
                    }
                    // Bool toggle on click.
                    if let PortValue::Bool(b) = prop.current {
                        let mut g = self.state.graph.lock().unwrap();
                        let _ = g.set_property(node_id, prop.name, PortValue::Bool(!b));
                        drop(g);
                        self.state.schedule_evaluate();
                        return EventResult::Consumed;
                    }
                }
                if let Some(node_id) = self.hit_node(&layouts, canvas_pos) {
                    if !modifiers.shift && !self.selected.contains(&node_id) {
                        self.selected.clear();
                    }
                    self.selected.insert(node_id);
                    self.update_display_node_for(node_id);
                    self.begin_drag_node(node_id, canvas_pos);
                    return EventResult::Consumed;
                }
                // Empty canvas — clear selection.
                if !modifiers.shift {
                    self.selected.clear();
                }
                EventResult::Consumed
            }
            MouseButton::Middle => {
                self.interaction = CanvasState::PanningCanvas {
                    start_offset: self.canvas_offset,
                    start_local: pos,
                };
                EventResult::Consumed
            }
            MouseButton::Right => {
                // Open the add-node popup at the cursor. Remember the
                // canvas-space position so the new node anchors there.
                self.popup_canvas_pos = canvas_pos;
                self.popup.open_at(pos);
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    }

    fn on_mouse_move(&mut self, pos: Point) -> EventResult {
        let canvas_pos = self.local_to_canvas(pos);
        match &mut self.interaction {
            CanvasState::PanningCanvas { start_offset, start_local } => {
                self.canvas_offset = [
                    start_offset[0] + (pos.x - start_local.x),
                    start_offset[1] + (pos.y - start_local.y),
                ];
                EventResult::Consumed
            }
            CanvasState::DraggingNode { ids, start_positions, start_canvas } => {
                let dx = canvas_pos[0] - start_canvas[0];
                let dy = canvas_pos[1] - start_canvas[1];
                let mut g = self.state.graph.lock().unwrap();
                for (id, p0) in ids.iter().zip(start_positions.iter()) {
                    let _ = g.set_position(*id, [p0[0] + dx, p0[1] + dy]);
                }
                EventResult::Consumed
            }
            CanvasState::DrawingConnection { cursor_canvas, .. } => {
                *cursor_canvas = canvas_pos;
                EventResult::Consumed
            }
            CanvasState::DraggingProperty { node_id, prop_name, start_value, start_local_x, min, max } => {
                // 1.0 unit per logical pixel; that's coarse for fine
                // tweaking, but adequate for Phase 4. Holding shift would
                // multiply sensitivity in a future iteration.
                let dx = pos.x - *start_local_x;
                let mut new_value = *start_value + dx;
                if let Some(mn) = *min { if new_value < mn { new_value = mn; } }
                if let Some(mx) = *max { if new_value > mx { new_value = mx; } }
                let id = *node_id;
                let name = *prop_name;
                let mut g = self.state.graph.lock().unwrap();
                let _ = g.set_property(id, name, PortValue::Number(new_value));
                drop(g);
                self.state.schedule_evaluate();
                EventResult::Consumed
            }
            CanvasState::Idle => EventResult::Ignored,
        }
    }

    fn on_mouse_up(&mut self, pos: Point, button: MouseButton, _modifiers: Modifiers) -> EventResult {
        let canvas_pos = self.local_to_canvas(pos);
        match (button, std::mem::replace(&mut self.interaction, CanvasState::Idle)) {
            (MouseButton::Left, CanvasState::DrawingConnection { from, from_socket_type, .. })
            | (MouseButton::Middle, CanvasState::DrawingConnection { from, from_socket_type, .. }) => {
                let layouts = self.snapshot_layouts();
                if let Some((node_id, socket)) = self.hit_socket(&layouts, canvas_pos) {
                    if node_id != from.node && socket.socket_type == from_socket_type {
                        // Determine direction.
                        let from_side = self.classify_socket(&from);
                        let target_side = Some(socket.side);
                        let (out_sid, in_sid) = match (from_side, target_side) {
                            (Some(SocketSide::Output), Some(SocketSide::Input)) => (
                                from.clone(),
                                SocketId { node: node_id, name: socket.name },
                            ),
                            (Some(SocketSide::Input), Some(SocketSide::Output)) => (
                                SocketId { node: node_id, name: socket.name },
                                from.clone(),
                            ),
                            _ => return EventResult::Consumed,
                        };
                        let edge = Edge { from: out_sid, to: in_sid.clone() };
                        let mut g = self.state.graph.lock().unwrap();
                        let mut connected = false;
                        match g.connect(edge.clone(), &self.state.registry) {
                            Ok(()) => connected = true,
                            Err(GraphError::InputAlreadyConnected) => {
                                let to_remove: Vec<Edge> =
                                    g.edges().iter().filter(|e| e.to == in_sid).cloned().collect();
                                for e in to_remove {
                                    let _ = g.disconnect(&e);
                                }
                                connected = g.connect(edge, &self.state.registry).is_ok();
                            }
                            Err(_) => {}
                        }
                        drop(g);
                        if connected {
                            self.state.schedule_evaluate();
                        }
                    }
                }
                EventResult::Consumed
            }
            (_, CanvasState::DraggingNode { .. })
            | (_, CanvasState::PanningCanvas { .. })
            | (_, CanvasState::DraggingProperty { .. }) => EventResult::Consumed,
            (_, _) => EventResult::Ignored,
        }
    }

    fn on_wheel(&mut self, pos: Point, delta_y: f64, _modifiers: Modifiers) -> EventResult {
        if delta_y == 0.0 {
            return EventResult::Ignored;
        }
        let canvas_before = self.local_to_canvas(pos);
        let factor = if delta_y > 0.0 { ZOOM_STEP } else { 1.0 / ZOOM_STEP };
        let new_scale = (self.canvas_scale * factor).clamp(ZOOM_MIN, ZOOM_MAX);
        if (new_scale - self.canvas_scale).abs() < 1e-9 {
            return EventResult::Consumed;
        }
        // Keep the cursor's canvas-space point pinned beneath the cursor.
        // canvas_offset_new = pos - canvas_before * new_scale
        self.canvas_offset = [
            pos.x - canvas_before[0] * new_scale,
            pos.y - canvas_before[1] * new_scale,
        ];
        self.canvas_scale = new_scale;
        EventResult::Consumed
    }

    fn on_key_down(&mut self, key: &Key, _mods: Modifiers) -> EventResult {
        match key {
            Key::Char(' ') => {
                self.space_held = true;
                EventResult::Consumed
            }
            Key::Delete => {
                if self.selected.is_empty() {
                    return EventResult::Ignored;
                }
                let to_remove: Vec<NodeId> = self.selected.drain().collect();
                let mut g = self.state.graph.lock().unwrap();
                for id in to_remove {
                    let _ = g.remove_node(id);
                }
                drop(g);
                self.state.schedule_evaluate();
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    }

    fn on_key_up(&mut self, key: &Key, _mods: Modifiers) -> EventResult {
        if let Key::Char(' ') = key {
            self.space_held = false;
            return EventResult::Consumed;
        }
        EventResult::Ignored
    }
}

/// Build the right-click "Add Node" menu — category-grouped submenus
/// containing every registered node type. Action ids are
/// `"add.{type_id}"` matching the top menu bar's convention.
fn build_add_node_popup_items(state: &AppState) -> Vec<MenuEntry> {
    let mut out = Vec::new();
    for (cat, defs) in state.registry.by_category() {
        if defs.is_empty() {
            continue;
        }
        let items = defs
            .iter()
            .map(|d| {
                MenuEntry::Item(MenuItem::action(
                    d.display_name(),
                    format!("add.{}", d.type_id()),
                ))
            })
            .collect();
        out.push(MenuEntry::Item(MenuItem::submenu(cat, items)));
    }
    out
}

/// Free function for tests / external callers — adds a node with default
/// properties at the given canvas position. Returns the new node id.
pub fn add_node_with_defaults(
    graph: &mut Graph,
    registry: &NodeRegistry,
    type_id: &'static str,
    position: [f64; 2],
) -> Option<NodeId> {
    let def = registry.get(type_id)?;
    let id = graph.allocate_id();
    let mut node = atomartist_lib::graph::node::NodeInstance::new(id, type_id, position);
    for prop in def.properties() {
        node.properties.insert(prop.name, prop.default);
    }
    graph.add_node(node).ok()?;
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use atomartist_lib::nodes;

    fn fixture() -> AppState {
        let mut reg = NodeRegistry::new();
        nodes::register_all(&mut reg);
        AppState::new(Graph::new(), reg)
    }

    #[test]
    fn local_to_canvas_round_trip_with_pan_and_zoom() {
        let state = fixture();
        let mut canvas = NodeCanvas::new(state);
        canvas.canvas_offset = [50.0, 30.0];
        canvas.canvas_scale = 1.5;
        let lp = Point::new(80.0, 60.0);
        let cp = canvas.local_to_canvas(lp);
        assert!((cp[0] - (80.0 - 50.0) / 1.5).abs() < 1e-9);
        assert!((cp[1] - (60.0 - 30.0) / 1.5).abs() < 1e-9);
    }

    #[test]
    fn add_node_helper_inserts_with_defaults() {
        let state = fixture();
        let id = {
            let mut g = state.graph.lock().unwrap();
            add_node_with_defaults(&mut g, &state.registry, "Box", [10.0, 20.0]).unwrap()
        };
        let g = state.graph.lock().unwrap();
        let n = g.get(id).unwrap();
        assert_eq!(n.type_id, "Box");
        assert!(n.properties.contains_key("width"));
    }
}
