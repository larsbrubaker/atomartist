//! Drawing helpers for the node canvas — split from `canvas_widget.rs`
//! to respect the 800-line limit.
//!
//! All coordinates are **canvas-space**: positive Y is up (agg-gui
//! convention), and a node's `position` is its top-left corner. The caller
//! is responsible for having `save()`d, `translate()`d by `canvas_offset`,
//! and `scale()`d by `canvas_scale` on the `DrawCtx` before invoking these
//! helpers, so we draw straight in canvas units.

use agg_gui::{Color, DrawCtx};

use atomartist_lib::graph::node::{NodeId, NodeInstance, PortValue};
use atomartist_lib::registry::{NodeDef, SocketDef};
use atomartist_lib::SocketType;

// --- Layout constants ------------------------------------------------------

pub const NODE_WIDTH: f64 = 180.0;
pub const TITLE_HEIGHT: f64 = 26.0;
pub const ROW_HEIGHT: f64 = 22.0;
pub const NODE_BOTTOM_PAD: f64 = 6.0;
pub const SOCKET_RADIUS: f64 = 5.5;
pub const SOCKET_HIT_RADIUS: f64 = 9.0;
pub const NODE_RADIUS: f64 = 6.0;

/// Side a socket appears on, in node-local coordinates (canvas Y-up).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SocketSide {
    Input,
    Output,
}

/// A single socket's hit-test info — its index within the input or output
/// list, its name, type, and computed canvas-space position.
#[derive(Clone, Debug)]
pub struct SocketLayout {
    pub side: SocketSide,
    pub name: &'static str,
    pub socket_type: SocketType,
    /// Canvas-space center of the socket circle.
    pub center: [f64; 2],
}

/// One editable property row inside a node — its name, current value
/// (read from the node at layout time), and canvas-space row rectangle.
#[derive(Clone, Debug)]
pub struct PropLayout {
    pub name: &'static str,
    /// Numeric range, copied from the `PropDef`. Used to clamp drag deltas.
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub current: PortValue,
    /// Canvas-space rect (top-left + size) of the row — used for hit testing.
    pub top_left: [f64; 2],
    pub size: [f64; 2],
}

impl PropLayout {
    pub fn contains(&self, canvas_pos: [f64; 2]) -> bool {
        let x0 = self.top_left[0];
        let y1 = self.top_left[1];
        let x1 = x0 + self.size[0];
        let y0 = y1 - self.size[1];
        canvas_pos[0] >= x0 && canvas_pos[0] <= x1 && canvas_pos[1] >= y0 && canvas_pos[1] <= y1
    }
}

/// Computed canvas-space layout for one node — its size and socket
/// positions. Recomputed on each paint frame; cheap.
#[derive(Clone, Debug)]
pub struct NodeLayoutInfo {
    pub node_id: NodeId,
    pub top_left: [f64; 2],
    pub size: [f64; 2],
    pub sockets: Vec<SocketLayout>,
    pub props: Vec<PropLayout>,
    pub display_name: &'static str,
    pub category: &'static str,
}

impl NodeLayoutInfo {
    /// Hit-test the node body (excluding socket overflow). Returns true if
    /// the canvas-space point lies inside the rounded body.
    pub fn body_contains(&self, canvas_pos: [f64; 2]) -> bool {
        let x0 = self.top_left[0];
        let y1 = self.top_left[1];
        let y0 = y1 - self.size[1];
        let x1 = x0 + self.size[0];
        canvas_pos[0] >= x0 && canvas_pos[0] <= x1 && canvas_pos[1] >= y0 && canvas_pos[1] <= y1
    }

    /// True if the canvas-space point lies inside the title bar (top
    /// `TITLE_HEIGHT` strip) — used to tell "click on header" apart from
    /// "click on a property widget" once Phase 4 lands.
    pub fn header_contains(&self, canvas_pos: [f64; 2]) -> bool {
        let x0 = self.top_left[0];
        let y1 = self.top_left[1];
        let y0 = y1 - TITLE_HEIGHT;
        let x1 = x0 + self.size[0];
        canvas_pos[0] >= x0 && canvas_pos[0] <= x1 && canvas_pos[1] >= y0 && canvas_pos[1] <= y1
    }

    /// Find a socket whose hit radius contains `canvas_pos`.
    pub fn socket_at(&self, canvas_pos: [f64; 2]) -> Option<&SocketLayout> {
        self.sockets.iter().find(|s| {
            let dx = s.center[0] - canvas_pos[0];
            let dy = s.center[1] - canvas_pos[1];
            dx * dx + dy * dy <= SOCKET_HIT_RADIUS * SOCKET_HIT_RADIUS
        })
    }

    /// Find the property row hit by `canvas_pos`.
    pub fn prop_at(&self, canvas_pos: [f64; 2]) -> Option<&PropLayout> {
        self.props.iter().find(|p| p.contains(canvas_pos))
    }
}

/// Compute layout for a single node. The node's `position` is treated as
/// its top-left in canvas-space. Sockets are stacked from the top under
/// the title bar; properties stack below the sockets.
pub fn layout_node(node: &NodeInstance, def: &dyn NodeDef) -> NodeLayoutInfo {
    let inputs = def.input_sockets();
    let outputs = def.output_sockets();
    let socket_rows = inputs.len().max(outputs.len()) as f64;

    let prop_defs = def.properties();
    let prop_rows = prop_defs.len() as f64;

    let height = TITLE_HEIGHT + socket_rows * ROW_HEIGHT + prop_rows * ROW_HEIGHT + NODE_BOTTOM_PAD;
    let top_left = [node.position[0], node.position[1]];

    let mut sockets = Vec::with_capacity(inputs.len() + outputs.len());
    push_sockets(&mut sockets, &inputs, SocketSide::Input, top_left);
    push_sockets(&mut sockets, &outputs, SocketSide::Output, top_left);

    let prop_section_top = top_left[1] - TITLE_HEIGHT - socket_rows * ROW_HEIGHT;
    let mut props = Vec::with_capacity(prop_defs.len());
    for (i, p) in prop_defs.iter().enumerate() {
        let row_top_y = prop_section_top - i as f64 * ROW_HEIGHT;
        let current = node.properties.get(p.name).cloned().unwrap_or_else(|| p.default.clone());
        props.push(PropLayout {
            name: p.name,
            min: p.min,
            max: p.max,
            current,
            top_left: [top_left[0] + 1.0, row_top_y],
            size: [NODE_WIDTH - 2.0, ROW_HEIGHT],
        });
    }
    NodeLayoutInfo {
        node_id: node.id,
        top_left,
        size: [NODE_WIDTH, height],
        sockets,
        props,
        display_name: def.display_name(),
        category: def.category(),
    }
}

fn push_sockets(
    out: &mut Vec<SocketLayout>,
    sockets: &[SocketDef],
    side: SocketSide,
    top_left: [f64; 2],
) {
    let x = match side {
        SocketSide::Input => top_left[0],
        SocketSide::Output => top_left[0] + NODE_WIDTH,
    };
    for (i, s) in sockets.iter().enumerate() {
        let y = top_left[1] - TITLE_HEIGHT - (i as f64 + 0.5) * ROW_HEIGHT;
        out.push(SocketLayout {
            side,
            name: s.name,
            socket_type: s.socket_type,
            center: [x, y],
        });
    }
}

// --- Drawing ---------------------------------------------------------------

/// Theme palette used by the canvas. Kept module-local so the canvas owns
/// its visual identity without forcing a global theme dependency.
pub struct CanvasPalette {
    pub canvas_bg: Color,
    pub canvas_grid: Color,
    pub node_body: Color,
    pub node_body_selected: Color,
    pub node_border: Color,
    pub node_title: Color,
    pub label_text: Color,
}

impl CanvasPalette {
    pub fn dark() -> Self {
        Self {
            canvas_bg: Color::rgb(0.13, 0.14, 0.16),
            canvas_grid: Color::rgba(1.0, 1.0, 1.0, 0.04),
            node_body: Color::rgb(0.22, 0.23, 0.27),
            node_body_selected: Color::rgb(0.28, 0.32, 0.38),
            node_border: Color::rgba(0.0, 0.0, 0.0, 0.5),
            node_title: Color::rgb(0.34, 0.40, 0.55),
            label_text: Color::rgb(0.92, 0.93, 0.95),
        }
    }
}

/// Draw an infinite grid backdrop. `cell_size` is in canvas units.
pub fn draw_canvas_grid(
    ctx: &mut dyn DrawCtx,
    visible: ([f64; 2], [f64; 2]),
    cell_size: f64,
    color: Color,
) {
    let (mn, mx) = visible;
    if mn[0] >= mx[0] || mn[1] >= mx[1] || cell_size <= 0.0 {
        return;
    }
    ctx.set_stroke_color(color);
    ctx.set_line_width(1.0);
    let x0 = (mn[0] / cell_size).floor() * cell_size;
    let mut x = x0;
    while x <= mx[0] {
        ctx.begin_path();
        ctx.move_to(x, mn[1]);
        ctx.line_to(x, mx[1]);
        ctx.stroke();
        x += cell_size;
    }
    let y0 = (mn[1] / cell_size).floor() * cell_size;
    let mut y = y0;
    while y <= mx[1] {
        ctx.begin_path();
        ctx.move_to(mn[0], y);
        ctx.line_to(mx[0], y);
        ctx.stroke();
        y += cell_size;
    }
}

fn socket_color(t: SocketType) -> Color {
    let [r, g, b, a] = t.display_color_rgba();
    Color::rgba(
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    )
}

/// Render one node into the canvas (caller has already applied pan/zoom).
pub fn draw_node(
    ctx: &mut dyn DrawCtx,
    layout: &NodeLayoutInfo,
    selected: bool,
    palette: &CanvasPalette,
) {
    let x = layout.top_left[0];
    let y_top = layout.top_left[1];
    let w = layout.size[0];
    let h = layout.size[1];
    let y_bot = y_top - h;

    // Body (rounded rect) — agg-gui rect uses bottom-left origin in Y-up,
    // so we pass (x, y_bot, w, h).
    ctx.set_fill_color(if selected { palette.node_body_selected } else { palette.node_body });
    ctx.begin_path();
    ctx.rounded_rect(x, y_bot, w, h, NODE_RADIUS);
    ctx.fill();

    // Title bar (filled rectangle at the top, taking up TITLE_HEIGHT).
    ctx.set_fill_color(palette.node_title);
    ctx.begin_path();
    ctx.rounded_rect(x, y_top - TITLE_HEIGHT, w, TITLE_HEIGHT, NODE_RADIUS);
    ctx.fill();
    // Cover the bottom corners of the title bar so it only rounds at the top.
    ctx.set_fill_color(if selected { palette.node_body_selected } else { palette.node_body });
    ctx.begin_path();
    ctx.rect(x, y_top - TITLE_HEIGHT, w, NODE_RADIUS);
    ctx.fill();
    // Re-draw the upper portion of the title bar to restore its color.
    ctx.set_fill_color(palette.node_title);
    ctx.begin_path();
    ctx.rect(x, y_top - TITLE_HEIGHT + NODE_RADIUS, w, TITLE_HEIGHT - NODE_RADIUS);
    ctx.fill();

    // Border around the whole node.
    ctx.set_stroke_color(palette.node_border);
    ctx.set_line_width(1.0);
    ctx.begin_path();
    ctx.rounded_rect(x, y_bot, w, h, NODE_RADIUS);
    ctx.stroke();

    // Title text — centered vertically in the title bar.
    ctx.set_fill_color(palette.label_text);
    ctx.set_font_size(13.0);
    let title_y = y_top - TITLE_HEIGHT * 0.5 - 4.0;
    ctx.fill_text(layout.display_name, x + 10.0, title_y);

    // Property rows — drawn before sockets so socket labels can paint on top.
    let prop_bg = Color::rgba(0.15, 0.16, 0.20, 0.9);
    for p in &layout.props {
        // Slight inset background for the row.
        ctx.set_fill_color(prop_bg);
        ctx.begin_path();
        ctx.rect(p.top_left[0], p.top_left[1] - p.size[1], p.size[0], p.size[1] - 2.0);
        ctx.fill();

        // Name on the left.
        ctx.set_fill_color(palette.label_text);
        ctx.set_font_size(11.0);
        ctx.fill_text(p.name, p.top_left[0] + 6.0, p.top_left[1] - 14.0);

        // Value on the right (rough right-align by string length estimate).
        let value_str = format_value(&p.current);
        let est = (value_str.len() as f64) * 6.0;
        ctx.fill_text(
            &value_str,
            p.top_left[0] + p.size[0] - est - 6.0,
            p.top_left[1] - 14.0,
        );
    }

    // Socket circles + labels.
    for s in &layout.sockets {
        let c = socket_color(s.socket_type);
        ctx.set_fill_color(c);
        ctx.begin_path();
        ctx.circle(s.center[0], s.center[1], SOCKET_RADIUS);
        ctx.fill();
        ctx.set_stroke_color(palette.node_border);
        ctx.set_line_width(1.0);
        ctx.begin_path();
        ctx.circle(s.center[0], s.center[1], SOCKET_RADIUS);
        ctx.stroke();

        ctx.set_fill_color(palette.label_text);
        ctx.set_font_size(11.0);
        let label_y = s.center[1] - 4.0;
        match s.side {
            SocketSide::Input => {
                ctx.fill_text(s.name, x + SOCKET_RADIUS * 2.0 + 4.0, label_y);
            }
            SocketSide::Output => {
                // For outputs, right-align by approximate width — agg-gui
                // doesn't expose right-anchored text so we offset by an
                // estimate. Refined when Phase 4 introduces measure_text.
                let est_width = (s.name.len() as f64) * 6.5;
                ctx.fill_text(
                    s.name,
                    x + w - est_width - SOCKET_RADIUS * 2.0 - 4.0,
                    label_y,
                );
            }
        }
    }
}

fn format_value(v: &PortValue) -> String {
    match v {
        PortValue::Number(n) => {
            if (n.fract()).abs() < 1e-6 {
                format!("{}", *n as i64)
            } else {
                format!("{:.3}", n)
            }
        }
        PortValue::Bool(b) => if *b { "true".into() } else { "false".into() },
        PortValue::StringVal(s) => s.as_str().to_string(),
        PortValue::Color(_) => "Color".into(),
        PortValue::Matrix4x4(_) => "Matrix4x4".into(),
        PortValue::Path2d(_) => "Path2d".into(),
        PortValue::Geometry3d(_) => "Geometry3d".into(),
        PortValue::None => "—".into(),
    }
}

/// Draw a cubic-bezier connection between two canvas-space socket centers.
/// The control points are offset horizontally so the curve always exits to
/// the right of an output and enters from the left of an input — like a
/// classic node-graph editor.
pub fn draw_bezier_connection(
    ctx: &mut dyn DrawCtx,
    from: [f64; 2],
    to: [f64; 2],
    color: Color,
    line_width: f64,
) {
    let dx = (to[0] - from[0]).abs().max(60.0).min(220.0);
    let cp1 = [from[0] + dx, from[1]];
    let cp2 = [to[0] - dx, to[1]];
    ctx.set_stroke_color(color);
    ctx.set_line_width(line_width);
    ctx.begin_path();
    ctx.move_to(from[0], from[1]);
    ctx.cubic_to(cp1[0], cp1[1], cp2[0], cp2[1], to[0], to[1]);
    ctx.stroke();
}

#[cfg(test)]
mod tests {
    use super::*;
    use atomartist_lib::graph::node::PortValue;
    use atomartist_lib::registry::{NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, SocketDef};

    struct TestDef;
    impl NodeDef for TestDef {
        fn type_id(&self) -> &'static str { "Test" }
        fn category(&self) -> &'static str { "Test" }
        fn input_sockets(&self) -> Vec<SocketDef> {
            vec![SocketDef::required("a", SocketType::Number)]
        }
        fn output_sockets(&self) -> Vec<SocketDef> {
            vec![SocketDef::required("out", SocketType::Number)]
        }
        fn evaluate(&self, _: &NodeInputs, _: &NodeProperties) -> Result<NodeOutputs, NodeError> {
            Ok(NodeOutputs::default())
        }
    }

    #[test]
    fn layout_places_input_left_output_right() {
        let mut node = NodeInstance::new(NodeId(1), "Test", [100.0, 200.0]);
        node.properties.insert("dummy", PortValue::None);
        let info = layout_node(&node, &TestDef);
        assert_eq!(info.top_left, [100.0, 200.0]);
        assert_eq!(info.sockets.len(), 2);
        let input = info.sockets.iter().find(|s| s.side == SocketSide::Input).unwrap();
        let output = info.sockets.iter().find(|s| s.side == SocketSide::Output).unwrap();
        assert!((input.center[0] - 100.0).abs() < 1e-9);
        assert!((output.center[0] - (100.0 + NODE_WIDTH)).abs() < 1e-9);
        // Input row centred at TITLE_HEIGHT + 0.5*ROW_HEIGHT below the top.
        let expected_y = 200.0 - TITLE_HEIGHT - 0.5 * ROW_HEIGHT;
        assert!((input.center[1] - expected_y).abs() < 1e-9);
    }

    #[test]
    fn body_and_header_contains() {
        let node = NodeInstance::new(NodeId(1), "Test", [0.0, 0.0]);
        let info = layout_node(&node, &TestDef);
        // Inside body
        assert!(info.body_contains([10.0, -10.0]));
        // Above body
        assert!(!info.body_contains([10.0, 10.0]));
        // In header (top strip)
        assert!(info.header_contains([10.0, -5.0]));
        // Below header
        assert!(!info.header_contains([10.0, -TITLE_HEIGHT - 5.0]));
    }

    #[test]
    fn socket_hit_test() {
        let node = NodeInstance::new(NodeId(1), "Test", [0.0, 0.0]);
        let info = layout_node(&node, &TestDef);
        let in_center = info.sockets[0].center;
        // Exact center hits.
        assert!(info.socket_at(in_center).is_some());
        // Slightly off center, within hit radius.
        assert!(info
            .socket_at([in_center[0] + 5.0, in_center[1] + 5.0])
            .is_some());
        // Far away misses.
        assert!(info.socket_at([in_center[0] + 50.0, in_center[1]]).is_none());
    }
}
