//! Ported from NodeDesigner's `tests/unit/change-detection.test.ts`.
//!
//! Covers [`ChangeTracker`] — JSON-snapshot diffing for "do we have
//! unsaved work?" prompts.
//!
//! ## Mapping notes — JS → Rust
//!
//! - JS used module-level globals (`savedGraphState`) and free
//!   functions; Rust uses a `ChangeTracker` instance so multiple
//!   projects / windows can each track their own baseline. JS's
//!   `clearSavedState()` maps to `ChangeTracker::clear`,
//!   `markCurrentStateAsSaved` to `mark_saved`, `hasUnsavedChanges` to
//!   `has_unsaved_changes`.
//! - JS-only surface area we deliberately skip:
//!   - `getGraphContentJson(null)` — Rust takes `&Graph` by reference,
//!     there is no nullable graph.
//!   - "strips runtime properties (`last_node_id`, `iteration`,
//!     `globaltime`)" — atomartist's `graph_to_json_string` never emits
//!     those fields. The canonical serializer in `graph_json.rs` is the
//!     single source of truth for what we persist; if it ships a
//!     value, it survives change-detection by design.
//!   - "normalizes pos as Float32Array / object to array" — Rust
//!     position is `[f64; 2]`, there is no equivalent ambiguity.
//!   - "removes runtime socket labels from inputs/outputs" — JS treats
//!     dynamic-node display labels as runtime cruft. atomartist treats
//!     them as persisted user state (the Output node's "Extrude -
//!     Geometry" label survives save/load). They participate in the
//!     dirty bit just like any other property; that's intentional —
//!     reordering a slot or retyping it via a connection should be
//!     considered an unsaved change.
//!   - "returns true when project name changed" — project name lives in
//!     the UI app state, not the engine's `Graph`. The UI layer is
//!     expected to OR its own project-name diff with the engine's
//!     `has_unsaved_changes`.

use atomartist_lib::graph::graph::Graph;
use atomartist_lib::graph::node::PortValue;
use atomartist_lib::serialization::{graph_json::graph_to_json_string, ChangeTracker};

#[path = "common/mod.rs"]
mod common;

use common::registry;

// ============================================================================
// graph_to_json_string — canonical, deterministic form
// ============================================================================

/// JS: getGraphContentJson "returns JSON string for valid graph"
#[test]
fn canonical_json_is_non_empty_for_populated_graph() {
    let reg = registry();
    let mut g = Graph::new();
    g.add_new_node("test::ProducerNumber", [100.0, 100.0], &reg).unwrap();

    let json = graph_to_json_string(&g);
    assert!(!json.is_empty());
    // Must round-trip through a JSON parser.
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(parsed.is_object());
}

/// Canonical form must be deterministic across runs — the change
/// tracker relies on byte-for-byte equality of the serialized output.
/// This is the property-side variant of the same guarantee
/// `graph_json.rs` already covers for node + noodle ordering.
#[test]
fn canonical_json_is_stable_across_calls() {
    let reg = registry();
    let mut g = Graph::new();
    let p = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    g.set_property(p, "value", PortValue::Number(3.0)).unwrap();
    g.set_property(p, "extra", PortValue::Number(7.0)).unwrap();

    let first = graph_to_json_string(&g);
    for _ in 0..8 {
        assert_eq!(graph_to_json_string(&g), first);
    }
}

// ============================================================================
// has_unsaved_changes
// ============================================================================

/// JS: hasUnsavedChanges "returns false when no saved state exists"
#[test]
fn no_baseline_means_no_unsaved_changes() {
    let reg = registry();
    let mut g = Graph::new();
    g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();

    let tracker = ChangeTracker::new();
    assert!(!tracker.has_baseline());
    assert!(!tracker.has_unsaved_changes(&g));
}

/// JS: hasUnsavedChanges "returns false when graph matches saved state"
#[test]
fn graph_matches_baseline_after_mark_saved() {
    let reg = registry();
    let mut g = Graph::new();
    g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();

    let mut tracker = ChangeTracker::new();
    tracker.mark_saved(&g);
    assert!(tracker.has_baseline());
    assert!(!tracker.has_unsaved_changes(&g));
}

/// JS: hasUnsavedChanges "returns true when graph content changed"
/// (we use a position change as the "content change" signal)
#[test]
fn moving_a_node_marks_graph_dirty() {
    let reg = registry();
    let mut g = Graph::new();
    let p = g.add_new_node("test::ProducerNumber", [100.0, 100.0], &reg).unwrap();

    let mut tracker = ChangeTracker::new();
    tracker.mark_saved(&g);

    g.set_position(p, [200.0, 200.0]).unwrap();
    assert!(tracker.has_unsaved_changes(&g));
}

/// JS: hasUnsavedChanges "returns true when a node is added"
#[test]
fn adding_a_node_marks_graph_dirty() {
    let reg = registry();
    let mut g = Graph::new();
    g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();

    let mut tracker = ChangeTracker::new();
    tracker.mark_saved(&g);

    g.add_new_node("test::ConsumerNumber", [200.0, 0.0], &reg).unwrap();
    assert!(tracker.has_unsaved_changes(&g));
}

/// JS: hasUnsavedChanges "returns true when a node is removed"
#[test]
fn removing_a_node_marks_graph_dirty() {
    let reg = registry();
    let mut g = Graph::new();
    let a = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    g.add_new_node("test::ConsumerNumber", [200.0, 0.0], &reg).unwrap();

    let mut tracker = ChangeTracker::new();
    tracker.mark_saved(&g);

    g.remove_node(a).unwrap();
    assert!(tracker.has_unsaved_changes(&g));
}

/// Adapted from JS: connecting nodes is a structural change that must
/// flip the dirty bit. JS covered this through its general "content
/// changed" test; here we make it explicit because connecting on the
/// Rust side runs `on_input_connected` hooks that may mutate sockets,
/// which is exactly the class of change we want to track.
#[test]
fn connecting_nodes_marks_graph_dirty() {
    use atomartist_lib::graph::graph::Noodle;
    let reg = registry();
    let mut g = Graph::new();
    let producer = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let consumer = g.add_new_node("test::ConsumerNumber", [200.0, 0.0], &reg).unwrap();

    let mut tracker = ChangeTracker::new();
    tracker.mark_saved(&g);

    let out = g.get(producer).unwrap().output_by_name("out").unwrap().uid;
    let in_uid = g.get(consumer).unwrap().input_by_name("in").unwrap().uid;
    g.connect(Noodle::new(producer, out, consumer, in_uid), &reg).unwrap();

    assert!(tracker.has_unsaved_changes(&g));
}

/// Atomic property mutation must show up in the diff.
#[test]
fn changing_a_property_marks_graph_dirty() {
    let reg = registry();
    let mut g = Graph::new();
    let p = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    g.set_property(p, "value", PortValue::Number(1.0)).unwrap();

    let mut tracker = ChangeTracker::new();
    tracker.mark_saved(&g);

    g.set_property(p, "value", PortValue::Number(2.0)).unwrap();
    assert!(tracker.has_unsaved_changes(&g));
}

// ============================================================================
// mark_saved
// ============================================================================

/// JS: markCurrentStateAsSaved "saves the current graph state"
#[test]
fn mark_saved_treats_current_graph_as_clean() {
    let reg = registry();
    let mut g = Graph::new();
    g.add_new_node("test::ProducerNumber", [100.0, 100.0], &reg).unwrap();

    let mut tracker = ChangeTracker::new();
    tracker.mark_saved(&g);
    assert!(!tracker.has_unsaved_changes(&g));
}

/// JS: markCurrentStateAsSaved "updates saved state when called again"
#[test]
fn mark_saved_replaces_previous_baseline() {
    let reg = registry();
    let mut g = Graph::new();
    let p = g.add_new_node("test::ProducerNumber", [100.0, 100.0], &reg).unwrap();

    let mut tracker = ChangeTracker::new();
    tracker.mark_saved(&g);

    g.set_position(p, [300.0, 300.0]).unwrap();
    assert!(tracker.has_unsaved_changes(&g));

    // Re-save baseline at the new position.
    tracker.mark_saved(&g);
    assert!(!tracker.has_unsaved_changes(&g));
}

// ============================================================================
// clear
// ============================================================================

/// JS: clearSavedState "clears the saved state"
#[test]
fn clear_drops_baseline() {
    let reg = registry();
    let mut g = Graph::new();
    g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();

    let mut tracker = ChangeTracker::new();
    tracker.mark_saved(&g);
    assert!(tracker.has_baseline());

    tracker.clear();
    assert!(!tracker.has_baseline());
    // After clearing, the tracker reports "clean" because it has no
    // baseline to compare against — same contract as JS.
    assert!(!tracker.has_unsaved_changes(&g));
}

// ============================================================================
// Round-trip safety: a loaded graph must match a saved one
// ============================================================================

/// Save → load → save must produce identical canonical JSON. Without
/// this property, change detection would flag a freshly-loaded project
/// as dirty.
#[test]
fn save_load_round_trip_is_clean() {
    use atomartist_lib::serialization::graph_json::graph_from_json_str;
    use atomartist_lib::graph::graph::Noodle;

    let reg = registry();
    let mut g = Graph::new();
    let producer = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let consumer = g.add_new_node("test::ConsumerNumber", [200.0, 50.0], &reg).unwrap();
    g.set_property(producer, "value", PortValue::Number(42.0)).unwrap();
    let out = g.get(producer).unwrap().output_by_name("out").unwrap().uid;
    let in_uid = g.get(consumer).unwrap().input_by_name("in").unwrap().uid;
    g.connect(Noodle::new(producer, out, consumer, in_uid), &reg).unwrap();

    let json_before = graph_to_json_string(&g);
    let result = graph_from_json_str(&json_before, &reg).unwrap();
    let json_after = graph_to_json_string(&result.graph);
    assert_eq!(json_before, json_after, "round-trip must be byte-identical");
}
