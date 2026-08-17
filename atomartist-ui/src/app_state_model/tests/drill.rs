//! Component drill-in tests for the `AppStateModel` adapter — entering
//! and exiting component templates, scoped undo, instance/template socket
//! reconciliation on exit, menu-Add routing, and the unsaved-changes
//! interaction with an active drill-in. Split out of the parent
//! `app_state_model/tests.rs` to keep that file under the project's
//! 800-line cap; the remaining adapter tests (node/property views) stay
//! there.

use super::super::*;
use atomartist_lib::nodes;
use atomartist_lib::registry::NodeRegistry;
use atomartist_lib::Graph;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

fn str_val(s: &str) -> PortValue {
    PortValue::StringVal(Arc::new(s.into()))
}

/// Build an `AppState` whose registry contains one component type
/// (`"Comp"`) backed by a template with a single Number input port
/// `"w"`. Returns the state, the shared template Arc, and the component
/// type id.
fn component_fixture() -> (AppState, Arc<Mutex<Graph>>, &'static str) {
    let mut reg = NodeRegistry::new();
    nodes::register_all(&mut reg);
    let template = Arc::new(Mutex::new(Graph::new()));
    {
        let mut t = template.lock().unwrap();
        let gin = t.add_new_node("GraphInput", [0.0, 0.0], &reg).unwrap();
        t.set_property_hooked(gin, "port_type", str_val("Number"), &reg)
            .unwrap();
        t.set_property(gin, "name", str_val("w")).unwrap();
    }
    let type_id = nodes::register_subgraph(&mut reg, "Comp", "Comp", template.clone());
    let state = AppState::new(Graph::new(), reg);
    (state, template, type_id)
}

/// The template's GraphInput node id — the sole node in the fresh
/// component template.
fn template_graph_input(state: &AppState) -> atomartist_lib::graph::node::NodeId {
    let ag = state.active_graph();
    let g = ag.lock().unwrap();
    let id = g
        .nodes()
        .find(|n| n.type_id.as_ref() == "GraphInput")
        .unwrap()
        .id;
    id
}

#[test]
fn enter_component_pushes_level_and_serves_template_nodes() {
    let (state, _template, type_id) = component_fixture();
    let inst = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node(type_id, [0.0, 0.0], &state.registry)
            .unwrap()
    };
    let bx = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node("Box", [0.0, 0.0], &state.registry).unwrap()
    };
    let mut model = AppStateModel::new(state);
    // A plain node is not a component — no drill-in.
    assert!(!ne::NodeGraphModel::on_node_activated(
        &mut model,
        ne::NodeId(bx.0)
    ));
    assert_eq!(model.state.edit_depth(), 0);
    // The component node drills in.
    assert!(ne::NodeGraphModel::on_node_activated(
        &mut model,
        ne::NodeId(inst.0)
    ));
    assert_eq!(model.state.edit_depth(), 1);
    // nodes() now serves the template — its single GraphInput node.
    let nodes = ne::NodeGraphModel::nodes(&model);
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].type_id, "GraphInput");
}

#[test]
fn exit_syncs_new_template_port_to_root_instance() {
    let (state, _template, type_id) = component_fixture();
    let inst = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node(type_id, [0.0, 0.0], &state.registry)
            .unwrap()
    };
    // Instance mints the single "w" input from the template scan.
    assert!(state
        .graph
        .lock()
        .unwrap()
        .get(inst)
        .unwrap()
        .input_by_name("w")
        .is_some());

    assert!(state.enter_component(inst));
    // Add a second Number port "h" to the template while drilled in.
    {
        let ag = state.active_graph();
        let mut g = ag.lock().unwrap();
        let gin = g
            .add_new_node("GraphInput", [0.0, 0.0], &state.registry)
            .unwrap();
        g.set_property_hooked(gin, "port_type", str_val("Number"), &state.registry)
            .unwrap();
        g.set_property(gin, "name", str_val("h")).unwrap();
    }
    // Exiting reconciles the root instance's socket layout.
    state.exit_to(0);
    assert_eq!(state.edit_depth(), 0);
    let g = state.graph.lock().unwrap();
    let sock = g
        .get(inst)
        .unwrap()
        .input_by_name("h")
        .expect("root instance gained the new 'h' socket on exit");
    assert_eq!(sock.socket_type, SocketType::Number);
}

#[test]
fn property_edit_inside_component_schedules_root_eval() {
    let (state, _template, type_id) = component_fixture();
    {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node(type_id, [0.0, 0.0], &state.registry)
            .unwrap();
    }
    // enter via the shared state so the model clone observes the stack.
    let inst = {
        let g = state.graph.lock().unwrap();
        let id = g
            .nodes()
            .find(|n| n.type_id.as_ref() == type_id)
            .unwrap()
            .id;
        id
    };
    assert!(state.enter_component(inst));
    let inst_gin = template_graph_input(&state);
    let mut model = AppStateModel::new(state.clone());
    let before = state.eval_ticket.load(Ordering::Relaxed);
    ne::NodeGraphModel::set_property(
        &mut model,
        ne::NodeId(inst_gin.0),
        "default_number",
        ne::PropertyValue::Number(3.0),
    );
    let after = state.eval_ticket.load(Ordering::Relaxed);
    assert!(
        after > before,
        "an in-component property edit must schedule a root re-evaluation",
    );
}

#[test]
fn undo_inside_component_leaves_root_history_untouched() {
    let (state, _template, type_id) = component_fixture();
    {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node(type_id, [0.0, 0.0], &state.registry)
            .unwrap();
    }
    let inst = {
        let g = state.graph.lock().unwrap();
        let id = g
            .nodes()
            .find(|n| n.type_id.as_ref() == type_id)
            .unwrap()
            .id;
        id
    };
    assert!(state.enter_component(inst));
    let inst_gin = template_graph_input(&state);
    let mut model = AppStateModel::new(state.clone());
    ne::NodeGraphModel::set_property(
        &mut model,
        ne::NodeId(inst_gin.0),
        "default_number",
        ne::PropertyValue::Number(5.0),
    );
    // The edit lands on the level's scoped undo stack, not the root's.
    assert_eq!(
        state.active_undo().lock().unwrap().undo_name(),
        Some("Change Property"),
    );
    assert_eq!(
        state.undo.lock().unwrap().undo_name(),
        None,
        "root undo history must be untouched by in-component edits",
    );
}

#[test]
fn on_node_activated_true_for_component_false_for_plain_node() {
    let (state, _template, type_id) = component_fixture();
    let inst = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node(type_id, [0.0, 0.0], &state.registry)
            .unwrap()
    };
    let num = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node("NumberConst", [0.0, 0.0], &state.registry)
            .unwrap()
    };
    let mut model = AppStateModel::new(state);
    assert!(!ne::NodeGraphModel::on_node_activated(
        &mut model,
        ne::NodeId(num.0)
    ));
    assert!(ne::NodeGraphModel::on_node_activated(
        &mut model,
        ne::NodeId(inst.0)
    ));
}

#[test]
fn menu_add_while_drilled_in_lands_in_template_not_root() {
    use crate::debug_windows::DebugWindowHandles;
    use crate::settings::DebugWindowsState;
    use crate::top_menu_bar::NoFileDialogs;

    let (state, template, type_id) = component_fixture();
    let inst = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node(type_id, [0.0, 0.0], &state.registry)
            .unwrap()
    };
    let root_before = state.graph.lock().unwrap().node_count();
    let template_before = template.lock().unwrap().node_count();

    assert!(state.enter_component(inst));
    let dialogs: std::sync::Arc<dyn crate::top_menu_bar::FileDialogProvider> =
        std::sync::Arc::new(NoFileDialogs);
    let debug = DebugWindowHandles::new(DebugWindowsState::default());
    crate::menu_actions::handle_action(&state, &dialogs, &debug, "add.Box");

    // Root graph is untouched; the Box landed in the visible template.
    assert_eq!(
        state.graph.lock().unwrap().node_count(),
        root_before,
        "menu Add must not touch the root while drilled in",
    );
    {
        let t = template.lock().unwrap();
        assert_eq!(t.node_count(), template_before + 1);
        assert!(
            t.nodes().any(|n| n.type_id.as_ref() == "Box"),
            "the added Box must land in the component template",
        );
    }

    // The Add landed on the drilled-in level's scoped undo stack, so it
    // is undoable — undoing removes the Box from the template again.
    let active_undo = state.active_undo();
    assert_eq!(
        active_undo.lock().unwrap().undo_name(),
        Some("Add Node"),
        "menu Add while drilled in must record an undoable command on the level's undo stack",
    );
    active_undo.lock().unwrap().undo();
    {
        let t = template.lock().unwrap();
        assert_eq!(
            t.node_count(),
            template_before,
            "undo must remove the added Box from the template",
        );
        assert!(
            !t.nodes().any(|n| n.type_id.as_ref() == "Box"),
            "the Box must be gone from the template after undo",
        );
    }
}

#[test]
fn drilled_in_reports_unsaved_changes_even_when_root_matches_baseline() {
    // A user editing a component template (edit_depth() > 0) must always
    // trip the unsaved-changes prompt, even though the change tracker
    // only watches the root graph. Otherwise File > New/Open would
    // silently discard the in-progress template edit.
    let (state, _template, type_id) = component_fixture();
    let inst = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node(type_id, [0.0, 0.0], &state.registry)
            .unwrap()
    };
    // Baseline on the current root so it is "clean" at depth 0.
    state.mark_saved_baseline();
    assert!(
        !state.has_unsaved_changes(),
        "root matches baseline at depth 0 — no unsaved changes",
    );

    assert!(state.enter_component(inst));
    assert!(
        state.has_unsaved_changes(),
        "being drilled into a component must report unsaved changes even \
         though the root graph still matches its baseline",
    );

    // Exiting with an unchanged root returns to clean.
    state.exit_to(0);
    assert_eq!(state.edit_depth(), 0);
    assert!(
        !state.has_unsaved_changes(),
        "after exiting to root with an unchanged root, no unsaved changes",
    );
}

#[test]
fn new_project_clears_drill_in_stack() {
    let (state, _template, type_id) = component_fixture();
    let inst = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node(type_id, [0.0, 0.0], &state.registry)
            .unwrap()
    };
    assert!(state.enter_component(inst));
    assert_eq!(state.edit_depth(), 1);

    let root = state.graph.clone();
    state.new_empty_project();

    assert_eq!(
        state.edit_depth(),
        0,
        "New must exit any drilled-in component"
    );
    assert!(
        Arc::ptr_eq(&state.active_graph(), &root),
        "active_graph() must resolve to the (new) root after New",
    );
    assert_eq!(state.active_graph().lock().unwrap().node_count(), 0);
}

/// Build a registry with two component types: `Inner` (empty template)
/// and `Outer` (template contains one `Inner` instance). Returns the
/// state, both template Arcs, and both type ids.
fn nested_component_fixture() -> (
    AppState,
    Arc<Mutex<Graph>>,
    Arc<Mutex<Graph>>,
    &'static str,
    &'static str,
) {
    let mut reg = NodeRegistry::new();
    nodes::register_all(&mut reg);
    let inner_tmpl = Arc::new(Mutex::new(Graph::new()));
    let inner_type = nodes::register_subgraph(&mut reg, "Inner", "Inner", inner_tmpl.clone());
    let outer_tmpl = Arc::new(Mutex::new(Graph::new()));
    {
        let mut t = outer_tmpl.lock().unwrap();
        t.add_new_node(inner_type, [0.0, 0.0], &reg).unwrap();
    }
    let outer_type = nodes::register_subgraph(&mut reg, "Outer", "Outer", outer_tmpl.clone());
    let state = AppState::new(Graph::new(), reg);
    (state, inner_tmpl, outer_tmpl, inner_type, outer_type)
}

/// Append a Number-typed GraphInput named `name` to `template`.
fn add_number_port(template: &Arc<Mutex<Graph>>, reg: &NodeRegistry, name: &str) {
    let mut t = template.lock().unwrap();
    let gin = t.add_new_node("GraphInput", [0.0, 0.0], reg).unwrap();
    t.set_property_hooked(gin, "port_type", str_val("Number"), reg)
        .unwrap();
    t.set_property(gin, "name", str_val(name)).unwrap();
}

#[test]
fn exit_to_root_after_nested_drill_syncs_each_parent() {
    let (state, inner_tmpl, outer_tmpl, inner_type, outer_type) = nested_component_fixture();
    let outer_inst = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node(outer_type, [0.0, 0.0], &state.registry)
            .unwrap()
    };
    // Make the root outer instance stale: add a port to the outer
    // template without reconciling the instance.
    add_number_port(&outer_tmpl, &state.registry, "ow");
    assert!(state
        .graph
        .lock()
        .unwrap()
        .get(outer_inst)
        .unwrap()
        .input_by_name("ow")
        .is_none());

    // Drill: outer template, then the inner instance inside it.
    assert!(state.enter_component(outer_inst));
    let inner_inst = {
        let ag = state.active_graph();
        let g = ag.lock().unwrap();
        let id = g
            .nodes()
            .find(|n| n.type_id.as_ref() == inner_type)
            .unwrap()
            .id;
        id
    };
    assert!(state.enter_component(inner_inst));
    assert_eq!(state.edit_depth(), 2);

    // Edit the inner template while drilled two levels deep.
    add_number_port(&inner_tmpl, &state.registry, "iw");

    // Pop both levels in one call.
    state.exit_to(0);
    assert_eq!(state.edit_depth(), 0);

    // Level-2 pop synced the OUTER template's Inner instance to the
    // inner template: it gained "iw".
    {
        let t = outer_tmpl.lock().unwrap();
        let sock = t
            .get(inner_inst)
            .unwrap()
            .input_by_name("iw")
            .expect("inner instance in outer template gained 'iw'");
        assert_eq!(sock.socket_type, SocketType::Number);
    }
    // Level-1 pop synced the ROOT's Outer instance to the outer
    // template: the previously-stale instance gained "ow".
    {
        let g = state.graph.lock().unwrap();
        let sock = g
            .get(outer_inst)
            .unwrap()
            .input_by_name("ow")
            .expect("root outer instance gained 'ow'");
        assert_eq!(sock.socket_type, SocketType::Number);
    }
}

#[test]
fn nested_drill_in_serves_inner_template() {
    let (state, inner_tmpl, _outer_tmpl, inner_type, outer_type) = nested_component_fixture();
    // Marker node inside the inner template so we can identify it.
    {
        let mut t = inner_tmpl.lock().unwrap();
        t.add_new_node("NumberConst", [0.0, 0.0], &state.registry)
            .unwrap();
    }
    let outer_inst = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node(outer_type, [0.0, 0.0], &state.registry)
            .unwrap()
    };
    let mut model = AppStateModel::new(state);

    // Drill into the outer component.
    assert!(ne::NodeGraphModel::on_node_activated(
        &mut model,
        ne::NodeId(outer_inst.0)
    ));
    // The outer template serves its single Inner instance.
    let inner_id = {
        let nodes = ne::NodeGraphModel::nodes(&model);
        let n = nodes.iter().find(|n| n.type_id == inner_type).unwrap();
        n.id
    };
    // Drill into the inner component.
    assert!(ne::NodeGraphModel::on_node_activated(&mut model, inner_id));
    assert_eq!(model.state.edit_depth(), 2);

    // nodes() now serves the inner template — its NumberConst marker,
    // and no longer the outer template's Inner instance.
    let nodes = ne::NodeGraphModel::nodes(&model);
    assert!(
        nodes.iter().any(|n| n.type_id == "NumberConst"),
        "inner template's marker node must be served",
    );
    assert!(
        !nodes.iter().any(|n| n.type_id == inner_type),
        "must not still be serving the outer template",
    );
}

/// `node_errors` — and `node_warnings`, which is keyed the same way —
/// use **root-graph** node ids, and a component template allocates ids
/// from its own space: here the template's node has *the same id* as the
/// failing root instance, which is exactly the collision the projection
/// has to refuse. While drilled in, nobody wears a badge of either
/// severity.
#[test]
fn drilled_in_nodes_never_wear_the_root_graphs_error_badges() {
    let (state, template, type_id) = component_fixture();
    let inst = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node(type_id, [0.0, 0.0], &state.registry)
            .unwrap()
    };
    let inner = template.lock().unwrap().nodes().next().unwrap().id;
    assert_eq!(inner, inst, "the id spaces really do collide");

    // The component instance is the failing node at the root — and a
    // degraded one, so the amber badge is under the same test.
    state
        .node_errors
        .lock()
        .unwrap()
        .insert(inst, "Comp: subgraph eval failed".to_string());
    state
        .node_warnings
        .lock()
        .unwrap()
        .insert(inst, "Comp: 1 of 3 parts were skipped".to_string());

    let mut model = AppStateModel::new(state);
    let at_root = ne::NodeGraphModel::nodes(&model);
    let root_view = at_root
        .iter()
        .find(|n| n.id.0 == inst.0)
        .expect("the instance is in the root projection");
    assert_eq!(
        root_view.error.as_deref(),
        Some("Comp: subgraph eval failed"),
        "the root instance is badged"
    );
    assert_eq!(
        root_view.warning.as_deref(),
        Some("Comp: 1 of 3 parts were skipped"),
        "and carries its warning too — the canvas picks which one shows"
    );

    assert!(ne::NodeGraphModel::on_node_activated(
        &mut model,
        ne::NodeId(inst.0)
    ));
    assert_eq!(model.state.edit_depth(), 1);

    let inside = ne::NodeGraphModel::nodes(&model);
    assert!(!inside.is_empty());
    assert!(
        inside.iter().all(|n| n.error.is_none()),
        "no error badges inside a component"
    );
    assert!(
        inside.iter().all(|n| n.warning.is_none()),
        "and no warning badges either — same colliding id space"
    );
}
