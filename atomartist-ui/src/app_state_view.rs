//! Per-project view state on [`AppState`]: capture at save, restore at
//! open, reset at File → New (design §5d step 6h-4).
//!
//! The format half lives in
//! [`atomartist_lib::serialization::view_state`]; this module is the
//! bridge between it and the three places the live view actually lives:
//!
//! | Piece      | Lives in                                   | Restored by |
//! |------------|--------------------------------------------|-------------|
//! | canvas     | the `NodeEditor` widget's private pan/zoom | a queued [`NodeEditorCommand::SetView`] |
//! | divider    | the `Splitter` widget                      | [`AppState::divider_ratio`] (adopted next layout) |
//! | camera     | the shared [`OrbitCamera`]                 | written directly |
//!
//! # "Deferred until layout settles" without a deferral
//!
//! NodeDesigner waits two `requestAnimationFrame`s before applying a
//! restored view, because its canvas has no size until the browser has
//! laid the page out. Our equivalent falls out of the architecture for
//! free: the canvas restore is a *command*, and the editor drains its
//! command queue at the top of `layout()` — the first moment the pane's
//! size is known, on whichever frame that turns out to be. The splitter
//! adopts its handle in `layout()` for the same reason. Neither needs a
//! frame counter, and neither can land against a zero-sized pane.
//!
//! # View moves never dirty the project
//!
//! Nothing here touches the graph, and the graph is the entire input to
//! [`ChangeTracker`](atomartist_lib::serialization::ChangeTracker) — view
//! state is composed into the file at write time by
//! [`write_project_to_bytes_with_view`](atomartist_lib::serialization::write_project_to_bytes_with_view)
//! and is invisible to the comparison. Panning, zooming, dragging the
//! splitter and orbiting the camera therefore leave a saved project
//! saved.

use atomartist_lib::serialization::{clamp_divider, CameraState, CanvasView, ProjectView};
use atomartist_renderer::CameraPose;

use crate::app_state::AppState;

impl AppState {
    /// Snapshot the live view for the project being written.
    ///
    /// The canvas numbers come from the mirrors the editor publishes on
    /// every pan / zoom ([`AppState::canvas_zoom`] / `canvas_pan`) rather
    /// than from the widget, which no non-widget code can reach.
    pub fn current_project_view(&self) -> ProjectView {
        let scale = *self.canvas_zoom.lock().unwrap();
        let offset = *self.canvas_pan.lock().unwrap();
        let framed = *self.camera_framed.lock().unwrap();
        let camera = self.camera.lock().unwrap().pose();
        ProjectView {
            view_state: Some(CanvasView { scale, offset }),
            divider_position: Some(clamp_divider(self.divider_ratio.get())),
            // Only meaningful once the document has actually been framed:
            // saving a project that has never shown geometry would
            // otherwise pin the default camera as its "initial" pose and
            // permanently suppress the auto-frame.
            camera_state: framed.map(|initial| CameraState {
                position: camera.position,
                target: camera.target,
                initial_position: Some(initial.position),
                initial_target: Some(initial.target),
            }),
        }
    }

    /// Adopt an opened project's view. Call once per open, with whatever
    /// the file carried — including `None`.
    ///
    /// Missing groups are *not* defaults: a project with no `view_state`
    /// leaves the canvas exactly where it is (NodeDesigner does not fit
    /// on open), and one with no `divider_position` leaves the splitter
    /// alone. A missing `camera_state`, though, does mean something: this
    /// document has never been framed, so the auto-frame is re-armed and
    /// the first geometry to arrive will be fitted, as it always was.
    pub fn apply_project_view(&self, view: Option<&ProjectView>) {
        // A degenerate `cameraState` (eye == target, or a non-finite
        // component) is treated as *no* camera at all rather than as a
        // restored one: `set_pose` refuses it, so recording the document
        // as "framed" anyway would pin the default pose forever and the
        // project would never auto-frame again.
        let applied = view.and_then(|v| v.camera_state).filter(|camera| {
            self.camera.lock().unwrap().set_pose(CameraPose {
                position: camera.position,
                target: camera.target,
            })
        });
        match applied {
            Some(camera) => {
                // Filling this slot is what stops the first evaluation's
                // auto-frame from throwing the restored camera away.
                // Falling back to the restored pose keeps that guarantee
                // for a file written without the initial pair — and a
                // degenerate *initial* pair falls back the same way, so
                // the slot never holds a pose the camera can't be put on.
                let initial = CameraPose {
                    position: camera.initial_position.unwrap_or(camera.position),
                    target: camera.initial_target.unwrap_or(camera.target),
                };
                let usable = initial.position != initial.target
                    && initial.position.iter().all(|c| c.is_finite())
                    && initial.target.iter().all(|c| c.is_finite());
                *self.camera_framed.lock().unwrap() = Some(if usable {
                    initial
                } else {
                    CameraPose {
                        position: camera.position,
                        target: camera.target,
                    }
                });
                self.mark_viewport_dirty();
            }
            None => self.reset_camera_positioning(),
        }
        if let Some(canvas) = view.and_then(|v| v.view_state) {
            // Both the widget (which owns the transform) and the mirrors
            // (which the favorites-bar drag reads) — the command lands a
            // frame later, and nothing should see a stale pair meanwhile.
            *self.canvas_zoom.lock().unwrap() = canvas.scale;
            *self.canvas_pan.lock().unwrap() = canvas.offset;
            self.node_editor
                .push(agg_gui_node_editor::NodeEditorCommand::SetView {
                    scale: canvas.scale,
                    offset: canvas.offset,
                });
        }
        if let Some(divider) = view.and_then(|v| v.divider_position) {
            self.divider_ratio.set(clamp_divider(divider));
        }
    }

    /// File → New: put the camera back to its default pose and re-arm the
    /// auto-frame, leaving the canvas pan/zoom and the splitter where the
    /// user had them (NodeDesigner's `newDesign` resets camera
    /// positioning only — the canvas view is workspace, not document).
    pub fn reset_camera_positioning(&self) {
        self.camera.lock().unwrap().reset_view();
        *self.camera_framed.lock().unwrap() = None;
        self.mark_viewport_dirty();
    }
}

#[cfg(test)]
#[path = "app_state_view_tests.rs"]
mod app_state_view_tests;
