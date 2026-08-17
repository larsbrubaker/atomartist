//! Per-node evaluation errors: the bridge from
//! [`EvalReport`](atomartist_lib::graph::executor::EvalReport) to the two
//! places the user can see them — the status-bar notice queue
//! ([`crate::storage_ops`]) and the canvas node badge
//! (`NodeView::error`, painted by the canvas's node widgets through
//! `agg_gui_node_editor::draw_error`).
//!
//! Warnings ride the same machinery in a second map (`node_warnings`):
//! same change-only posting rule, same pruning, and since B-5a the same
//! badge shape in amber instead of red.
//!
//! Owned by [`AppState`](crate::AppState) as `node_errors` and written by
//! the evaluator task in `app_state.rs`; read by
//! [`crate::app_state_model::node_views`] when it projects the graph for
//! the canvas.
//!
//! # Why the map, and not "just post a notice"
//!
//! Evaluation runs on every property drag sample, so a graph that stays
//! broken would post the same sentence dozens of times per second. Two
//! mechanisms stop that:
//!
//! 1. The executor settles the dirty flags even for a failed node, so a
//!    *parked* broken graph re-evaluates nothing at all.
//! 2. This module posts only what **changed** since the previous pass: a
//!    node that fails with the same message it failed with last time is
//!    silent. A node that starts failing, or fails differently, speaks
//!    once.
//!
//! Recovery is silent by the same rule — a fixed node simply drops out of
//! the map (clearing its badge) and posts nothing, because "it works now"
//! is not news worth a status-bar slot.
//!
//! # One more collapse happens downstream
//!
//! Two *different* nodes failing with the same sentence in the same pass
//! post two notices here, and
//! [`push_notice`](crate::storage_ops::push_notice) then drops the second
//! as a consecutive duplicate — the user sees the sentence once. That is
//! the intended reading (the status bar shows one line; saying the same
//! thing twice tells the user nothing), and it is pinned by
//! `two_nodes_failing_with_the_same_text_say_it_once`. The badges are
//! unaffected: both nodes are in the map, so both are badged, which is
//! where "*which* nodes" gets answered.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use atomartist_lib::graph::executor::EvalReport;
use atomartist_lib::graph::node::NodeId;
use atomartist_lib::registry::NodeRegistry;

use crate::storage_ops::{push_notice, Notices};
use crate::NoticeLevel;

/// Message per currently-failing node, shared with clones of `AppState`.
pub type NodeErrors = Arc<Mutex<HashMap<NodeId, String>>>;

/// The read side of the two maps, on [`AppState`](crate::AppState) itself
/// because that is where callers hold them — but written here, next to
/// the rules that fill them (and because `app_state.rs` sits at the
/// 800-line cap).
impl crate::AppState {
    /// Snapshot of every currently-failing node, keyed by node id, with
    /// the message the canvas badges in red and the status bar shows.
    pub fn node_errors_snapshot(&self) -> HashMap<NodeId, String> {
        snapshot(&self.node_errors)
    }

    /// Snapshot of every node whose last evaluation was *degraded* —
    /// output exists, but something was skipped or rescued. Badged
    /// amber, so a permanently degraded node stays visible long after
    /// its status-bar notice has scrolled away.
    pub fn node_warnings_snapshot(&self) -> HashMap<NodeId, String> {
        snapshot(&self.node_warnings)
    }
}

fn snapshot(map: &NodeErrors) -> HashMap<NodeId, String> {
    map.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Build the user-facing message set for one pass: the node's own error
/// text, prefixed with the node type's display name so the sentence
/// stands on its own in a status bar ("Boolean: input 'b' is not a closed
/// solid").
pub fn messages_for(report: &EvalReport, registry: &NodeRegistry) -> HashMap<NodeId, String> {
    report
        .failures
        .iter()
        .map(|f| {
            let label = registry
                .get(&f.type_id)
                .map(|def| def.display_name().to_string())
                .unwrap_or_else(|| f.type_id.to_string());
            (f.node, format!("{}: {}", label, f.message))
        })
        .collect()
}

/// The user-facing message set for one pass's **warnings**: nodes that
/// evaluated fine but had something to say. A node may raise several, so
/// they are joined into one sentence per node — the map is keyed by node
/// because that is what the change-detection rule below needs.
pub fn warnings_for(report: &EvalReport, registry: &NodeRegistry) -> HashMap<NodeId, String> {
    let mut out: HashMap<NodeId, Vec<String>> = HashMap::new();
    for w in &report.warnings {
        let label = registry
            .get(&w.type_id)
            .map(|def| def.display_name().to_string())
            .unwrap_or_else(|| w.type_id.to_string());
        // The Boolean node already prefixes its own messages, and
        // "Boolean: Boolean: …" is not a sentence.
        let text = if w.message.starts_with(&format!("{}:", label)) {
            w.message.clone()
        } else {
            format!("{}: {}", label, w.message)
        };
        out.entry(w.node).or_default().push(text);
    }
    out.into_iter()
        .map(|(id, messages)| (id, messages.join(" ")))
        .collect()
}

/// What one evaluation pass did, in the terms this module needs.
pub struct PassOutcome<'a> {
    /// Message per node that failed this pass.
    pub failures: HashMap<NodeId, String>,
    /// Message per node that succeeded with something to report. Kept
    /// apart from [`failures`](Self::failures) all the way to the
    /// status bar and the canvas: a warning badges the node *amber*
    /// (`NodeView::warning`), never red, because red means "this node
    /// produced nothing and everything downstream is blocked", which is
    /// exactly what a degraded result is not. A node in both maps wears
    /// the error badge — the canvas resolves that.
    pub warnings: HashMap<NodeId, String>,
    /// Nodes that evaluated *successfully* this pass — their stale error
    /// is dropped.
    pub succeeded: &'a [NodeId],
    /// Every node currently in the graph. Entries for anything else are
    /// pruned, so deleting a broken node takes its error with it (the
    /// node never "succeeds", so nothing else would ever remove it).
    pub live: &'a HashSet<NodeId>,
}

/// Fold one pass's outcome into the error set and post a notice for each
/// message that is new or has changed since the last pass.
///
/// Nodes that neither failed nor succeeded keep their previous state,
/// because `evaluate_dirty` walks only part of the graph — a
/// still-broken node that this pass never touched must keep its badge.
///
/// Any-thread safe: it takes the two mutexes and [`push_notice`] signals
/// the shell's waker, which is what lets the native background evaluator
/// report from off the UI thread.
pub fn record(
    errors: &NodeErrors,
    warnings: &NodeErrors,
    notices: &Notices,
    pass: PassOutcome<'_>,
) {
    // Warnings first, so that when a pass produces both the louder error
    // is the newer notice and wins the status-bar slot.
    post_changed(
        warnings,
        notices,
        NoticeLevel::Warning,
        pass.warnings,
        // A node that succeeded *without* warning this pass drops its
        // stale warning — but a node that succeeded *with* one is in
        // `succeeded` too, and `extend` below puts it straight back.
        pass.succeeded,
        pass.live,
    );
    post_changed(
        errors,
        notices,
        NoticeLevel::Error,
        pass.failures,
        pass.succeeded,
        pass.live,
    );
}

/// The shared body of [`record`], for one severity: fold this pass's
/// messages into `previous`, drop the entries that no longer apply, and
/// post only what is new or changed.
fn post_changed(
    previous: &NodeErrors,
    notices: &Notices,
    level: NoticeLevel,
    messages: HashMap<NodeId, String>,
    succeeded: &[NodeId],
    live: &HashSet<NodeId>,
) {
    let mut changed: Vec<(NodeId, String)> = {
        let mut previous = previous.lock().unwrap_or_else(|e| e.into_inner());
        let changed = messages
            .iter()
            .filter(|(id, message)| previous.get(id) != Some(message))
            .map(|(id, message)| (*id, message.clone()))
            .collect();
        for id in succeeded {
            previous.remove(id);
        }
        previous.retain(|id, _| live.contains(id));
        previous.extend(messages);
        changed
    };
    // Node order, so two nodes failing in the same pass report in a
    // stable sequence rather than the hash map's.
    changed.sort_by_key(|(id, _)| id.0);
    for (_, message) in changed {
        push_notice(notices, level, message);
    }
}

#[cfg(test)]
#[path = "eval_errors_tests.rs"]
mod eval_errors_tests;
