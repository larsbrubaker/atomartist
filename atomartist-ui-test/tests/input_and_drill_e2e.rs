//! End-to-end coverage for AtomArtist's Input nodes, typed subgraph
//! components, and component drill-in navigation — driven through the
//! real production widget tree via `TestHarness`.
//!
//! These features are AtomArtist-specific and have **no direct
//! NodeDesigner source counterpart** (NodeDesigner's LiteGraph fork has
//! no typed GraphInput / unified Output / drill-in editing model), so —
//! per the CLAUDE.md "cite the source" convention — there is nothing to
//! cross-reference. The nearest lib/model-level unit coverage lives in
//! `atomartist-lib/src/nodes/input/*`, `atomartist-lib/src/nodes/
//! subgraph_node.rs`, and `atomartist-ui/src/app_state_model/tests.rs`;
//! this file exercises the same behaviour through synthetic mouse / key
//! events against the live `build_app` tree.
//!
//! ## How real clicks reach a node row
//!
//! The node-editor bakes each node's pan/zoom-transformed bounds into a
//! child `NodeWidget` tree, and the harness `snapshot()` surfaces those
//! child widgets with absolute Y-up `screen_bounds`. We locate a node
//! (`NodeWidget` with a matching `node_id` property) and its interior
//! rows (`ValueEditorWidget` with a matching `property`, spatially
//! contained in the node) from the snapshot, then convert the widget's
//! world-space centre into a top-down screen click — the same flip
//! (`DEFAULT_HEIGHT - world_y`) the breadcrumb tests use.

use std::sync::{Arc, Mutex};

use agg_gui::{Key, MouseButton, Rect};
use atomartist_lib::geometry::bounds as mesh_bounds;
use atomartist_lib::graph::node::{NodeId, PortValue};
use atomartist_lib::graph::Noodle;
use atomartist_lib::nodes::{register_all, register_subgraph};
use atomartist_lib::registry::NodeRegistry;
use atomartist_lib::socket_types::SocketType;
use atomartist_lib::Graph;
use atomartist_ui::breadcrumb_bar::BACK_BUTTON_CENTER_X;
use atomartist_ui::menu_actions::confirm_discard_unsaved;
use atomartist_ui::top_menu_bar::NoFileDialogs;
use atomartist_ui::AppState;
use atomartist_ui_test::harness::DEFAULT_HEIGHT;
use atomartist_ui_test::TestHarness;

// ── snapshot / coordinate helpers ────────────────────────────────────────

/// Height of a node's title bar in canvas-space (mirrors the node-editor
/// crate's private `draw::TITLE_HEIGHT`; kept in sync for hit-point math).
const TITLE_HEIGHT: f64 = 26.0;

/// Absolute (Y-up) screen bounds of the `NodeWidget` for `node_id`.
fn node_bounds(h: &TestHarness, node_id: NodeId) -> Option<Rect> {
    let want = node_id.0.to_string();
    h.snapshot().into_iter().find_map(|n| {
        if n.type_name != "NodeWidget" {
            return None;
        }
        let matches = n
            .properties
            .iter()
            .any(|(k, v)| *k == "node_id" && *v == want);
        matches.then_some(n.screen_bounds)
    })
}

fn center(r: Rect) -> (f64, f64) {
    (r.x + r.width * 0.5, r.y + r.height * 0.5)
}

fn contains_center(outer: Rect, inner: Rect) -> bool {
    let (cx, cy) = center(inner);
    cx >= outer.x && cx <= outer.x + outer.width && cy >= outer.y && cy <= outer.y + outer.height
}

/// Find the `ValueEditorWidget` for `(node_id, property)` and return its
/// absolute Y-up screen bounds. Rows are matched to their node by spatial
/// containment (the snapshot is a flat list; the row widget's centre must
/// fall inside the node body).
fn value_editor(h: &TestHarness, node_id: NodeId, property: &str) -> Option<Rect> {
    let nb = node_bounds(h, node_id)?;
    h.snapshot().into_iter().find_map(|n| {
        if n.type_name != "ValueEditorWidget" {
            return None;
        }
        let is_prop = n
            .properties
            .iter()
            .any(|(k, v)| *k == "property" && v == property);
        (is_prop && contains_center(nb, n.screen_bounds)).then_some(n.screen_bounds)
    })
}

/// The Debug string of a value-editor row's resolved `EditorKind`
/// (surfaced by `ValueEditorWidget::properties`). Lets tests assert on a
/// live slider's min/max without reaching into the node-editor's private
/// `PropertyView`.
fn value_editor_kind(h: &TestHarness, node_id: NodeId, property: &str) -> Option<String> {
    let nb = node_bounds(h, node_id)?;
    h.snapshot().into_iter().find_map(|n| {
        if n.type_name != "ValueEditorWidget" {
            return None;
        }
        let is_prop = n
            .properties
            .iter()
            .any(|(k, v)| *k == "property" && v == property);
        if !(is_prop && contains_center(nb, n.screen_bounds)) {
            return None;
        }
        n.properties
            .iter()
            .find(|(k, _)| *k == "editor_kind")
            .map(|(_, v)| v.clone())
    })
}

/// Absolute Y-up screen bounds of the breadcrumb bar.
fn breadcrumb_bounds(h: &TestHarness) -> Rect {
    h.snapshot()
        .into_iter()
        .find(|n| n.type_name == "BreadcrumbBar")
        .expect("breadcrumb bar in tree")
        .screen_bounds
}

fn breadcrumb_prop(h: &TestHarness, name: &str) -> String {
    h.snapshot()
        .into_iter()
        .find(|n| n.type_name == "BreadcrumbBar")
        .and_then(|n| {
            n.properties
                .into_iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v)
        })
        .unwrap_or_default()
}

/// Convert a world-space (Y-up) point at fractional position `(fx, fy)`
/// of `sb` into a top-down screen click coordinate.
fn screen_point(sb: Rect, fx: f64, fy: f64) -> (f64, f64) {
    let wx = sb.x + sb.width * fx;
    let wy = sb.y + sb.height * fy;
    (wx, DEFAULT_HEIGHT - wy)
}

/// Left-click the centre of a widget given its Y-up world bounds.
fn click_widget(h: &mut TestHarness, sb: Rect) {
    let (x, y) = screen_point(sb, 0.5, 0.5);
    h.click(x, y, MouseButton::Left);
}

/// Screen point on a node's title bar (a few px below the top edge,
/// centred so we clear the collapse chevron on the far left).
fn title_bar_point(sb: Rect) -> (f64, f64) {
    let wx = sb.x + sb.width * 0.5;
    // World top edge = sb.y + height; drop half a title bar down into it.
    let wy = sb.y + sb.height - TITLE_HEIGHT * 0.5;
    (wx, DEFAULT_HEIGHT - wy)
}

/// Force a relayout so the canvas rebuilds its child NodeWidgets from the
/// (mutated) model. Mirrors the drill-in chrome tests' nudge.
fn relayout(h: &mut TestHarness) {
    h.mouse_move(1.0, 1.0);
}

fn str_val(s: &str) -> PortValue {
    PortValue::StringVal(Arc::new(s.into()))
}

/// Read a node's cached output value by socket name after evaluation.
fn cached_output(h: &TestHarness, id: NodeId, socket: &str) -> Option<PortValue> {
    let g = h.state().graph.lock().unwrap();
    let n = g.get(id)?;
    let uid = n.output_by_name(socket)?.uid;
    n.cached_outputs.get(&uid).cloned()
}

/// Union Z extent (max − min) across every body in the mesh output.
fn output_z_extent(h: &TestHarness) -> Option<f32> {
    let out = h.state().last_mesh_output.lock().unwrap().clone()?;
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for body in out.iter() {
        if let Some((mn, mx)) = mesh_bounds(&body.mesh) {
            lo = lo.min(mn[2]);
            hi = hi.max(mx[2]);
        }
    }
    (hi >= lo).then_some(hi - lo)
}

// ── 1. Input-node round trip: StringConst inline text editor ──────────────

#[test]
fn string_const_inline_editor_commits_on_enter_and_reverts_on_escape() {
    let mut h = TestHarness::new();
    // Seed a StringConst near the middle-left of the (Y-up) canvas so the
    // whole node lands inside the visible canvas pane.
    let id = {
        let mut g = h.state().graph.lock().unwrap();
        g.add_new_node("StringConst", [120.0, 170.0], &h.state().registry)
            .unwrap()
    };
    relayout(&mut h);

    // The `value` property row must surface as an inline-editable row.
    let row = value_editor(&h, id, "value").expect("StringConst value row present on canvas");

    // Real click opens the floating single-line text editor.
    click_widget(&mut h, row);
    assert!(
        h.find_by_type("TextField").is_some(),
        "clicking the string row must open the inline TextField editor overlay",
    );

    // Type through real key events, then commit with Enter.
    for c in "Hi".chars() {
        h.key_down(Key::Char(c));
    }
    h.key_down(Key::Enter);

    // The graph property carries the committed text.
    {
        let g = h.state().graph.lock().unwrap();
        match g.get(id).unwrap().properties.get("value") {
            Some(PortValue::StringVal(s)) => assert_eq!(s.as_str(), "Hi"),
            other => panic!("value not committed as StringVal: {other:?}"),
        }
    }
    // ...and evaluation carries it downstream (the node's `out` socket).
    h.evaluate_now();
    match cached_output(&h, id, "out") {
        Some(PortValue::StringVal(s)) => assert_eq!(s.as_str(), "Hi"),
        other => panic!("evaluation output must carry the committed string, got {other:?}"),
    }

    // Escape path: reopen, type something else, then cancel — the live
    // preview must revert to the pre-edit value ("Hi").
    relayout(&mut h);
    let row = value_editor(&h, id, "value").expect("value row still present");
    click_widget(&mut h, row);
    assert!(h.find_by_type("TextField").is_some(), "editor reopened");
    for c in "Zz".chars() {
        h.key_down(Key::Char(c));
    }
    h.key_down(Key::Escape);

    let g = h.state().graph.lock().unwrap();
    match g.get(id).unwrap().properties.get("value") {
        Some(PortValue::StringVal(s)) => assert_eq!(
            s.as_str(),
            "Hi",
            "Escape must revert the live-previewed edit to the pre-edit value",
        ),
        other => panic!("expected StringVal after escape, got {other:?}"),
    }
}

// ── 2. BoolConst toggle click + undo ─────────────────────────────────────

#[test]
fn bool_const_toggle_click_flips_and_undo_restores() {
    let mut h = TestHarness::new();
    let id = {
        let mut g = h.state().graph.lock().unwrap();
        g.add_new_node("BoolConst", [120.0, 170.0], &h.state().registry)
            .unwrap()
    };
    relayout(&mut h);

    // Fresh BoolConst defaults to `true`.
    let read_bool = |h: &TestHarness| -> bool {
        let g = h.state().graph.lock().unwrap();
        matches!(
            g.get(id).unwrap().properties.get("value"),
            Some(PortValue::Bool(true))
        )
    };
    assert!(read_bool(&h), "BoolConst starts true");

    // Real click on the toggle row flips it (through the model's undoable
    // set_property path).
    let row = value_editor(&h, id, "value").expect("BoolConst value row present");
    click_widget(&mut h, row);
    assert!(!read_bool(&h), "toggle click must flip the bool to false");
    assert_eq!(
        h.state().undo.lock().unwrap().undo_name(),
        Some("Change Property"),
        "the toggle must land an undoable command on the root stack",
    );

    // Undo restores the original value.
    //
    // LIMITATION: a synthetic Ctrl+Z chord does not reach the menu bar's
    // accelerator handler through the headless harness — some widget in
    // the reverse-paint-order traversal consumes the key before the
    // MenuBar's `on_unconsumed_key` runs (verified: the dispatch reports
    // Consumed but `edit.undo` never fires). We therefore drive the exact
    // production undo the `edit.undo` menu action performs
    // (`state.active_undo().undo()`) directly, rather than through the
    // keyboard accelerator.
    h.state().active_undo().lock().unwrap().undo();

    assert!(
        read_bool(&h),
        "undo must restore the toggle's original value",
    );
}

// ── 3. NumberConst clamps on eval + live slider range ────────────────────

#[test]
fn number_const_clamps_value_and_value_row_tracks_live_range() {
    let mut h = TestHarness::new();
    let id = {
        let mut g = h.state().graph.lock().unwrap();
        g.add_new_node("NumberConst", [120.0, 180.0], &h.state().registry)
            .unwrap()
    };
    // `min` / `max` / `step` are `advanced` (hidden) rows, so they can't be
    // driven by a canvas drag — set them through the graph API, the same
    // PortValue writes the production ChangePropertyCmd performs. Use an
    // INVERTED pair to also prove the sort. value is set above the range.
    {
        let mut g = h.state().graph.lock().unwrap();
        g.set_property(id, "min", PortValue::Number(12.0)).unwrap();
        g.set_property(id, "max", PortValue::Number(-3.0)).unwrap();
        g.set_property(id, "value", PortValue::Number(999.0)).unwrap();
    }

    // Evaluation clamps into the SORTED range [-3, 12] → 12.
    h.evaluate_now();
    match cached_output(&h, id, "out") {
        Some(PortValue::Number(n)) => assert_eq!(n, 12.0, "value clamps to the sorted max"),
        other => panic!("expected clamped Number, got {other:?}"),
    }

    // The value row's live slider range follows the (sorted) min/max.
    relayout(&mut h);
    let kind = value_editor_kind(&h, id, "value")
        .expect("NumberConst value row present with an editor kind");
    assert!(
        kind.contains("Slider"),
        "value row must render as a Slider, got {kind}",
    );
    // Sorted bounds surface as min: Some(-3.0) BEFORE max: Some(12.0).
    let min_at = kind.find("-3.0").expect("sorted min -3.0 in slider range");
    let max_at = kind.find("12.0").expect("sorted max 12.0 in slider range");
    assert!(
        min_at < max_at,
        "slider range must be sorted (min before max): {kind}",
    );
}

// ── 4. Component end-to-end: instantiate, override, drill-in, exit-sync ───

/// Build an `AppState` whose registry carries a `WidgetComp` component:
/// `Rectangle → Extrude.Paths`, `GraphInput(Number "height", default 5) →
/// Extrude.Height`, `Extrude.Geometry → Output`. The published output is a
/// Geometry3d whose Z extent equals the (injected) height. Returns the
/// state, the shared template, and the component type id.
fn component_state() -> (AppState, Arc<Mutex<Graph>>, &'static str) {
    let mut reg = NodeRegistry::new();
    register_all(&mut reg);

    let template = Arc::new(Mutex::new(Graph::new()));
    {
        let mut t = template.lock().unwrap();
        let rect = t.add_new_node("Rectangle", [0.0, 0.0], &reg).unwrap();
        let gin = t.add_new_node("GraphInput", [0.0, 200.0], &reg).unwrap();
        let ext = t.add_new_node("Extrude", [220.0, 0.0], &reg).unwrap();
        let out = t.add_new_node("Output", [440.0, 0.0], &reg).unwrap();

        t.set_property_hooked(gin, "port_type", str_val("Number"), &reg)
            .unwrap();
        t.set_property(gin, "name", str_val("height")).unwrap();
        t.set_property(gin, "default_number", PortValue::Number(5.0))
            .unwrap();

        let connect = |t: &mut Graph, from: NodeId, fs: &str, to: NodeId, ts: &str| {
            let fu = t.get(from).unwrap().output_by_name(fs).unwrap().uid;
            let tu = t.get(to).unwrap().input_by_name(ts).unwrap().uid;
            t.connect(Noodle::new(from, fu, to, tu), &reg).unwrap();
        };
        connect(&mut t, rect, "out", ext, "Paths");
        connect(&mut t, gin, "out", ext, "Height");
        // Extrude.Geometry → Output's trailing empty slot (adopts the name).
        let ext_uid = t.get(ext).unwrap().output_by_name("Geometry").unwrap().uid;
        let out_in = t.get(out).unwrap().inputs[0].uid;
        t.connect(Noodle::new(ext, ext_uid, out, out_in), &reg)
            .unwrap();
    }

    let type_id = register_subgraph(&mut reg, "WidgetComp", "Widget Comp", template.clone());
    let state = AppState::new(Graph::new(), reg);
    (state, template, type_id)
}

/// Name of the component instance's geometry output socket.
fn geometry_output_name(g: &Graph, id: NodeId) -> Option<String> {
    g.get(id)?
        .outputs
        .iter()
        .find(|s| s.socket_type == SocketType::Geometry3d)
        .map(|s| s.name.to_string())
}

#[test]
fn component_instance_evaluates_default_then_drills_in_and_syncs_on_exit() {
    let (state, template, type_id) = component_state();

    // Seed the root graph: instance (visible on canvas) wired into Output.
    let inst = {
        let mut g = state.graph.lock().unwrap();
        let inst = g.add_new_node(type_id, [150.0, 190.0], &state.registry).unwrap();
        let out = g.add_new_node("Output", [420.0, 190.0], &state.registry).unwrap();
        let geo = geometry_output_name(&g, inst).expect("instance publishes a geometry output");
        let fu = g.get(inst).unwrap().output_by_name(&geo).unwrap().uid;
        let out_in = g.get(out).unwrap().inputs[0].uid;
        g.connect(Noodle::new(inst, fu, out, out_in), &state.registry)
            .unwrap();
        inst
    };

    let mut h = TestHarness::with_app_state(state);
    relayout(&mut h);

    // Default eval: height defaults to 5 → mesh Z extent ≈ 5.
    h.evaluate_now();
    let default_z = output_z_extent(&h).expect("component output produces geometry");
    assert!(
        (default_z - 5.0).abs() < 0.5,
        "default height 5 should give a ~5-tall extrusion, got {default_z}",
    );

    // Real slider drag on the instance's `height` input row raises the
    // override; re-eval reflects the taller extrusion.
    let row = value_editor(&h, inst, "height").expect("instance height row on canvas");
    let (fx, fy) = screen_point(row, 0.5, 0.5);
    h.drag((fx, fy), (fx + 60.0, fy), MouseButton::Left);
    h.evaluate_now();
    let overridden_z = output_z_extent(&h).expect("still produces geometry after override");
    assert!(
        overridden_z > default_z + 20.0,
        "raising the height override must grow the extrusion: {default_z} -> {overridden_z}",
    );

    // Real double-click on the instance title bar drills into the template.
    let nb = node_bounds(&h, inst).expect("instance node on canvas");
    let (tx, ty) = title_bar_point(nb);
    h.click(tx, ty, MouseButton::Left);
    h.click(tx, ty, MouseButton::Left);
    assert_eq!(h.state().edit_depth(), 1, "double-click drills into the component");
    relayout(&mut h);
    assert_eq!(
        breadcrumb_prop(&h, "trail"),
        "Top Level > Widget Comp",
        "breadcrumb reflects the drilled-in component's display name",
    );

    // Edit the template interface while drilled in: add a second Number
    // port `depth`.
    {
        let ag = state_active_graph(&h);
        let mut g = ag.lock().unwrap();
        let gin = g.add_new_node("GraphInput", [0.0, 400.0], &h.state().registry).unwrap();
        g.set_property_hooked(gin, "port_type", str_val("Number"), &h.state().registry)
            .unwrap();
        g.set_property(gin, "name", str_val("depth")).unwrap();
    }
    // Keep `template` referenced so the shared Arc's lifetime is explicit.
    assert!(Arc::strong_count(&template) >= 1);

    // Real breadcrumb back-button click exits one level and reconciles the
    // root instance's sockets against the edited template.
    let ticket_before = h.state().eval_ticket.load(std::sync::atomic::Ordering::Relaxed);
    let sb = breadcrumb_bounds(&h);
    let wx = sb.x + BACK_BUTTON_CENTER_X;
    let wy = sb.y + sb.height * 0.5;
    h.click(wx, DEFAULT_HEIGHT - wy, MouseButton::Left);

    assert_eq!(h.state().edit_depth(), 0, "back button exits to the root");
    {
        let g = h.state().graph.lock().unwrap();
        let sock = g
            .get(inst)
            .unwrap()
            .input_by_name("depth")
            .expect("root instance gained the synced 'depth' socket on exit");
        assert_eq!(sock.socket_type, SocketType::Number);
    }
    let ticket_after = h.state().eval_ticket.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        ticket_after > ticket_before,
        "exiting the component must schedule a root re-evaluation",
    );
}

/// Borrow the harness state's active graph (the component template while
/// drilled in). Split out so the closure above stays readable.
fn state_active_graph(h: &TestHarness) -> Arc<Mutex<Graph>> {
    h.state().active_graph()
}

// ── 5. File > New while drilled in clears the stack ──────────────────────

#[test]
fn file_new_while_drilled_in_clears_stack_and_root() {
    let (state, _template, type_id) = component_state();
    let inst = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node(type_id, [150.0, 190.0], &state.registry).unwrap()
    };

    let mut h = TestHarness::with_app_state(state);
    relayout(&mut h);

    // Drill in via a real double-click on the instance title bar.
    let nb = node_bounds(&h, inst).expect("instance node on canvas");
    let (tx, ty) = title_bar_point(nb);
    h.click(tx, ty, MouseButton::Left);
    h.click(tx, ty, MouseButton::Left);
    assert_eq!(h.state().edit_depth(), 1, "drilled into the component");

    // Invoke the File > New action through the production routing the
    // menu's `file.new` arm runs (`handle_action` is pub(crate); its arm
    // is exactly this pair of public calls, and `NoFileDialogs` answers
    // the unsaved prompt with Discard).
    let dialogs = NoFileDialogs;
    if confirm_discard_unsaved(h.state(), &dialogs) {
        h.state().new_empty_project();
    }

    assert_eq!(h.state().edit_depth(), 0, "New must clear the drill-in stack");
    assert_eq!(
        h.state().graph.lock().unwrap().node_count(),
        0,
        "New serves an empty root graph",
    );
    // The canvas now serves the empty root — no node widgets remain.
    relayout(&mut h);
    assert!(
        node_bounds(&h, inst).is_none(),
        "the old instance must be gone from the canvas after New",
    );
}
