//! Component drill-in navigation on [`AppState`] — the *state* half of
//! double-click-to-edit-a-component (the breadcrumb chrome lands in a
//! later step). Split from `app_state.rs` to keep both files under the
//! 800-line cap.
//!
//! The model (`AppStateModel`) and every graph-mutating command redirect
//! to [`AppState::active_graph`] / [`AppState::active_undo`] so editing a
//! component drives the component's template, not the root graph. The
//! 3-D viewport and the evaluator deliberately stay on the root graph, so
//! the live model updates while the user edits inside a component.

use std::sync::{Arc, Mutex};

use agg_gui::undo::UndoBuffer;
use atomartist_lib::graph::node::NodeId;
use atomartist_lib::Graph;

use crate::app_state::{AppState, EditLevel};

impl AppState {
    /// The graph currently being edited — the top drill-in level's
    /// template, or the root graph when nothing is drilled in.
    pub fn active_graph(&self) -> Arc<Mutex<Graph>> {
        match self.edit_stack.lock().unwrap().last() {
            Some(level) => level.graph.clone(),
            None => self.graph.clone(),
        }
    }

    /// The undo stack for the graph currently being edited — the top
    /// level's scoped stack, or the root stack.
    pub fn active_undo(&self) -> Arc<Mutex<UndoBuffer>> {
        match self.edit_stack.lock().unwrap().last() {
            Some(level) => level.undo.clone(),
            None => self.undo.clone(),
        }
    }

    /// Current drill-in depth — `0` at the root, `1` inside one
    /// component, and so on.
    pub fn edit_depth(&self) -> usize {
        self.edit_stack.lock().unwrap().len()
    }

    /// Attempt to drill into `node_id` (resolved in the *active* graph).
    /// Succeeds only when the node's type is a component whose
    /// `subgraph_template()` is `Some`; on success pushes a fresh
    /// [`EditLevel`] (with its own empty undo stack) and returns `true`.
    /// A non-component node returns `false` and leaves the stack
    /// untouched.
    ///
    /// Locks are short and scoped: the active-graph lock is released
    /// before the stack lock is taken, so this never holds two graph
    /// locks at once (safe to call from the node editor's
    /// `on_node_activated`, which holds the model mutex but none of
    /// these).
    pub fn enter_component(&self, node_id: NodeId) -> bool {
        let type_id = {
            let g = self.active_graph();
            let g = g.lock().unwrap();
            match g.get(node_id) {
                Some(n) => n.type_id.clone(),
                None => return false,
            }
        };
        let def = match self.registry.get(&type_id) {
            Some(d) => d.clone(),
            None => return false,
        };
        let template = match def.subgraph_template() {
            Some(t) => t,
            None => return false,
        };
        let level = EditLevel {
            label: def.display_name().to_string(),
            type_id: type_id.to_string(),
            graph: template,
            undo: Arc::new(Mutex::new(UndoBuffer::new())),
        };
        self.edit_stack.lock().unwrap().push(level);
        true
    }

    /// Truncate the drill-in stack to `depth` levels (`0` = back to the
    /// root graph). For every level popped, reconcile the parent graph's
    /// instances of that level's template — adding / removing / retyping
    /// their sockets to match the (possibly edited) template interface —
    /// then schedule a root re-evaluation so the change is reflected.
    ///
    /// Each popped level's undo history is discarded (v1 simplification —
    /// component-edit undo is scoped to the drilled-in session).
    pub fn exit_to(&self, depth: usize) {
        loop {
            let popped = {
                let mut stack = self.edit_stack.lock().unwrap();
                if stack.len() <= depth {
                    break;
                }
                stack.pop()
            };
            let level = match popped {
                Some(l) => l,
                None => break,
            };
            // After the pop, `active_graph()` is the parent (the new top
            // level, or the root) — the graph whose instances reference
            // the template we just left.
            let parent = self.active_graph();
            {
                let mut pg = parent.lock().unwrap();
                atomartist_lib::nodes::sync_instances_to_template(
                    &mut pg,
                    &self.registry,
                    &level.graph,
                );
            }
        }
        self.schedule_evaluate();
    }

    /// Pop a single drill-in level (the breadcrumb "back" action).
    /// No-op at the root.
    pub fn exit_one(&self) {
        let target = self.edit_stack.lock().unwrap().len().saturating_sub(1);
        self.exit_to(target);
    }

    /// Schedule a root re-evaluation after a graph edit. When drilled
    /// into a component, first mark the root-graph instances of the
    /// component being edited dirty, so the live template change
    /// propagates to the 3-D viewport while the user is still inside the
    /// component. When not drilled in, this is just a plain
    /// [`AppState::schedule_evaluate`].
    pub fn schedule_evaluate_after_edit(&self) {
        self.mark_active_template_instances_dirty();
        self.schedule_evaluate();
    }

    /// Mark every root-graph instance of the currently-edited component's
    /// template dirty so the next evaluation recomputes it. No-op when not
    /// drilled in.
    ///
    /// Note (deferred): only the *top-of-stack* template's root instances
    /// are marked. Editing a component nested inside another component
    /// won't live-update the 3-D view until exit, because the root
    /// instance is of the outer component, not the inner template. That's
    /// acceptable for v1 single-level drill-in.
    fn mark_active_template_instances_dirty(&self) {
        let template = match self.edit_stack.lock().unwrap().last() {
            Some(level) => level.graph.clone(),
            None => return,
        };
        let mut root = self.graph.lock().unwrap();
        let ids: Vec<NodeId> = root
            .nodes()
            .filter_map(|n| {
                let def = self.registry.get(&n.type_id)?;
                let tpl = def.subgraph_template()?;
                if Arc::ptr_eq(&tpl, &template) {
                    Some(n.id)
                } else {
                    None
                }
            })
            .collect();
        for id in ids {
            root.mark_dirty_subtree(id);
        }
    }
}
