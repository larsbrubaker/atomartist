//! Per-project *view* state — where the user had the node canvas, the
//! splitter and the 3-D camera when they last saved.
//!
//! Ported from NodeDesigner, which stores the same three groups inside
//! its design JSON (`viewState`, `dividerPosition`, `cameraState`). The
//! rationale carries over unchanged: a view belongs to the *document*,
//! not to the application — reopening a project should put you back where
//! you were, and two projects open in two windows should not fight over
//! one global "last camera".
//!
//! ## Why this is not part of `save_graph`
//!
//! [`ProjectView`] rides along in [`GraphFile::view`](super::graph_json::GraphFile)
//! but is written only by the *project* encoder
//! ([`write_project_to_bytes_with_view`](super::atmr::write_project_to_bytes_with_view)),
//! never by `save_graph`. That is deliberate and load-bearing: the
//! unsaved-changes detector ([`ChangeTracker`](super::ChangeTracker))
//! compares `graph_to_json_string`, so anything `save_graph` emits marks
//! the project dirty. Panning the canvas or orbiting the camera must
//! never do that, so view state is composed in at write time and the
//! comparison payload never sees it.
//!
//! ## Schema
//!
//! `.atmr` is pre-release, so these fields are folded straight into the
//! v1 `graph.json` rather than versioned separately. Every one of them is
//! optional on read: a project written before this step (or by a tool
//! that doesn't know about views) opens with `view: None` and the
//! application keeps its current view — see the restore rules in
//! `atomartist-ui`'s `app_state_view`.
//!
//! ```json
//! {
//!   "version": 1,
//!   "nodes": [ ... ],
//!   "noodles": [ ... ],
//!   "view": {
//!     "view_state": { "scale": 0.85, "offset": [120.0, -40.0] },
//!     "divider_position": 0.6,
//!     "camera_state": {
//!       "position": [60.0, -80.0, 45.0],
//!       "target": [0.0, 0.0, 0.0],
//!       "initial_position": [60.0, -80.0, 45.0],
//!       "initial_target": [0.0, 0.0, 0.0]
//!     }
//!   }
//! }
//! ```

use serde::{Deserialize, Serialize};

/// Bounds NodeDesigner clamps its divider percentage to (20 % … 80 % of
/// the window), expressed here as the fraction of the height given to the
/// preview (top) pane.
///
/// **Deliberately narrower than the widget's own drag range.** agg-gui's
/// [`SplitterRatio`](agg_gui::SplitterRatio) allows 5 % … 95 %, because
/// shoving a pane almost shut while working is a legitimate thing to do.
/// What a *document* may dictate is narrower: reopening a project that
/// pins the canvas to a 5 % sliver looks like a broken window, and the
/// user has no obvious way to tell that the file did it. So the drag
/// stays free and only the persisted value is clamped — a project saved
/// at 5 % reopens at 20 %, which is ND's rule too.
pub const DIVIDER_MIN: f64 = 0.20;
pub const DIVIDER_MAX: f64 = 0.80;

/// Everything about "where the user was looking" that a project carries.
///
/// Each group is independently optional: a project may restore the canvas
/// without ever having had a camera worth saving, and a reader that only
/// understands one of them can ignore the rest.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct ProjectView {
    /// Node-canvas pan / zoom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_state: Option<CanvasView>,
    /// Splitter position as the fraction of the height given to the 3-D
    /// (top) pane, clamped to [`DIVIDER_MIN`]..=[`DIVIDER_MAX`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub divider_position: Option<f64>,
    /// 3-D viewport camera.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_state: Option<CameraState>,
}

impl ProjectView {
    /// True when there is nothing worth writing — the encoder skips the
    /// whole entry in that case, keeping bytes identical to a project
    /// saved before this feature existed.
    pub fn is_empty(&self) -> bool {
        self.view_state.is_none() && self.divider_position.is_none() && self.camera_state.is_none()
    }
}

/// Node-canvas transform: `local = canvas * scale + offset`.
///
/// `offset` is in agg-gui's **Y-up** widget-local pixels (origin at the
/// pane's bottom-left), which is the editor's own convention — the value
/// round-trips through [`NodeEditor::pan`](agg_gui_node_editor::NodeEditor)
/// untouched. NodeDesigner's equivalent is top-down; nothing converts,
/// because nothing shares the number across the two apps.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct CanvasView {
    pub scale: f64,
    pub offset: [f64; 2],
}

/// 3-D camera as two world-space points, plus the pose the document was
/// *first* framed at.
///
/// `initial_position` / `initial_target` are NodeDesigner's
/// `initialPosition` / `initialTarget`: the auto-frame the design got
/// when its geometry first appeared. Restoring them is what tells the
/// viewport this document has already been framed, so opening a saved
/// project doesn't get its camera thrown away by the auto-frame on the
/// first evaluation.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct CameraState {
    pub position: [f32; 3],
    pub target: [f32; 3],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_position: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_target: Option<[f32; 3]>,
}

/// Clamp a divider fraction into NodeDesigner's 20…80 % window.
pub fn clamp_divider(position: f64) -> f64 {
    if position.is_finite() {
        position.clamp(DIVIDER_MIN, DIVIDER_MAX)
    } else {
        DIVIDER_MIN
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_view_is_recognised_as_nothing_to_write() {
        assert!(ProjectView::default().is_empty());
        let with_divider = ProjectView {
            divider_position: Some(0.5),
            ..Default::default()
        };
        assert!(!with_divider.is_empty());
    }

    /// Every group is optional on read — a blob carrying only one of
    /// them (or none) must still parse.
    #[test]
    fn partial_json_deserialises() {
        let only_camera: ProjectView = serde_json::from_str(
            r#"{"camera_state":{"position":[1.0,2.0,3.0],"target":[0.0,0.0,0.0]}}"#,
        )
        .expect("partial view parses");
        assert!(only_camera.view_state.is_none());
        assert!(only_camera.divider_position.is_none());
        let camera = only_camera.camera_state.expect("camera present");
        assert_eq!(camera.position, [1.0, 2.0, 3.0]);
        assert!(camera.initial_position.is_none());

        let nothing: ProjectView = serde_json::from_str("{}").expect("empty view parses");
        assert!(nothing.is_empty());
    }

    #[test]
    fn divider_is_clamped_to_nodedesigners_window() {
        assert_eq!(clamp_divider(0.5), 0.5);
        assert_eq!(clamp_divider(0.01), DIVIDER_MIN);
        assert_eq!(clamp_divider(0.99), DIVIDER_MAX);
        assert_eq!(clamp_divider(f64::NAN), DIVIDER_MIN);
    }
}
