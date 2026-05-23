//! Track whether the live graph differs from the last "saved" state.
//!
//! Sits on top of [`graph_to_json_string`](super::graph_json::graph_to_json_string)
//! — the canonical serialized form of the graph. A [`ChangeTracker`]
//! caches the JSON string captured at the last save, and answers
//! `has_unsaved_changes` by re-serializing and comparing.
//!
//! Ported from NodeDesigner's `static/js/node-editor/core/change-detection.js`.
//! JS used module-level globals + a `getElementById("project-name")` DOM
//! read so the comparison covered the user-visible project name. The
//! atomartist engine has no project-name concept — that lives in the UI
//! layer's app state. The UI is free to compose its own dirty bit out of
//! this tracker plus its own project-name diff.
//!
//! ## Why JSON-string comparison?
//!
//! Cheap, deterministic given the BTreeMap-backed property maps in
//! `graph_json`, and lossless for any change that survives a save/load
//! round trip. We pay the cost of an extra serialize on every check, but
//! the size of a typical graph is small relative to a frame's worth of
//! rendering and the check happens at human-interactive moments (file
//! menu opening, close-window prompt), not in the render loop.

use crate::graph::graph::Graph;
use crate::serialization::graph_json::graph_to_json_string;

/// Snapshot-based "unsaved changes" detector for a single [`Graph`].
///
/// Construct one per project and feed it the live graph at save points
/// and at every check. The tracker holds only the saved-state JSON, not a
/// reference to the graph — callers retain ownership.
#[derive(Debug, Default, Clone)]
pub struct ChangeTracker {
    /// JSON of the graph the last time `mark_saved` was called, or
    /// `None` if no baseline has been established yet.
    baseline: Option<String>,
}

impl ChangeTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a snapshot of the current graph state as the new baseline.
    /// Call this right after a successful save / load — anything that
    /// changes after this point will be reported as unsaved.
    pub fn mark_saved(&mut self, graph: &Graph) {
        self.baseline = Some(graph_to_json_string(graph));
    }

    /// True if the graph's canonical JSON differs from the last
    /// `mark_saved` snapshot. Returns `false` when no baseline has been
    /// established — matches the JS behavior: a freshly-opened editor
    /// session has nothing to be "unsaved" against until the user has
    /// either saved once or loaded a file.
    pub fn has_unsaved_changes(&self, graph: &Graph) -> bool {
        match &self.baseline {
            None => false,
            Some(baseline) => graph_to_json_string(graph) != *baseline,
        }
    }

    /// Forget the saved baseline. Subsequent `has_unsaved_changes` will
    /// return `false` until the next `mark_saved`.
    pub fn clear(&mut self) {
        self.baseline = None;
    }

    /// True when a baseline has been captured. Useful for tests and for
    /// UI that wants to disable a "discard changes" prompt when no save
    /// has yet occurred.
    pub fn has_baseline(&self) -> bool {
        self.baseline.is_some()
    }
}
