//! `CameraDrag` — the viewport's left/right/middle-button drag state
//! machine. Extracted from `viewport_widget.rs` so that file stays
//! under the repository's 800-line guardrail as new gizmo drag variants
//! land (each control gizmo adds a variant + its per-frame fields).
//!
//! The state transitions live in
//! `viewport_widget/viewport_widget_interactions.rs` (`on_mouse_down` /
//! `on_mouse_move` / `on_mouse_up`); the per-frame drag math lives in
//! `viewport_widget/body_drag.rs`. This module only declares the shape
//! of the state each variant carries.

use agg_gui::Point;
use atomartist_lib::graph::node::NodeId;

#[derive(Clone, Debug)]
pub(super) enum CameraDrag {
    None,
    /// Right-drag (or modifier-aware left-drag) → orbit. Tracks the
    /// previous cursor sample so each `MouseMove` can feed an
    /// incremental delta into `OrbitCamera::orbit_drag`, which then
    /// branches on `orbit_mode` (Turntable vs Trackball). The
    /// previous absolute-delta scheme always behaved like turntable
    /// regardless of mode — see `OrbitCamera::orbit_drag` for the
    /// per-mode math.
    Orbit { last_local: Point },
    /// MatterCAD-style pan: each `MouseMove` re-intersects the
    /// stored `hit_plane` with the previous and current cursor rays
    /// and shifts the camera centre by the world delta, so the
    /// original world point under the cursor follows the cursor
    /// across the drag.
    Pan { last_local: Point },
    /// Left-button down — pending click-or-drag. Carries the body
    /// pick + bed-plane anchor done at mouse-down so the click case
    /// AND the drag promotion both have the data they need without
    /// re-picking:
    ///
    /// * Mouse-up while `!moved` → select `picked_body` (or clear
    ///   selection when `None`). The click case ALWAYS selects what
    ///   was under the cursor regardless of whether the body's
    ///   matrix is writable.
    /// * Mouse-move past 2-px threshold + writable matrix → promote
    ///   to `DragBodyXY` for bed-plane translation. If the matrix
    ///   isn't writable (rare — only nodes without a `matrix`
    ///   property), the drag stays in `Selecting` and the mouse-up
    ///   path still works — selection lands but no translation is
    ///   performed.
    Selecting {
        start_local: Point,
        moved: bool,
        picked_body: Option<NodeId>,
        /// Bed-plane (Z=0) intersection of the mouse-down ray.
        /// `None` when the camera was pointing parallel to the bed
        /// or the ray missed. Required to start `DragBodyXY` on a
        /// promotion — without an anchor the drag delta math has
        /// nothing to subtract.
        anchor_bed_pt: Option<[f32; 3]>,
    },
    /// Ctrl + Alt + Left-drag — zoom by vertical drag distance (matches
    /// MatterCAD's modifier-only zoom path).
    Zooming { start_local: Point, start_radius: f32 },
    /// Left-button down landed on a renderable body — pending body-XY
    /// drag. Becomes a click-select on mouse-up if `moved == false`,
    /// otherwise each `MouseMove` projects the cursor ray onto the
    /// bed plane (`Z = 0`) and translates the body's matrix by
    /// `current_bed_pt - anchor_bed_pt`. MatterCAD's `TranslateObject3D`
    /// + NodeDesigner's bed-plane drag both follow this pattern.
    DragBodyXY {
        /// Node id whose `matrix` property gets mutated each frame.
        node_id: NodeId,
        start_local: Point,
        moved: bool,
        /// Bed-plane intersection of the mouse-down ray. Drag delta
        /// is `current - anchor_bed_pt` so the world point that was
        /// under the cursor at drag start stays under the cursor.
        anchor_bed_pt: [f32; 3],
        /// Matrix snapshot at drag start — translation deltas land on
        /// this baseline so a coalesced drag undoes back to here.
        start_matrix: [f32; 16],
    },
    /// Left-button down landed on the Z-control sphere handle. Each
    /// `MouseMove` projects the cursor ray onto the world vertical
    /// line through `(anchor_xy[0], anchor_xy[1], *)` — the closest
    /// point's world Z becomes the body's new `matrix.tz`. MatterCAD's
    /// `MoveInZControl` follows the same skew-line projection.
    DragBodyZ {
        node_id: NodeId,
        start_local: Point,
        /// XY of the body's anchor (the gizmo sphere's XY when the
        /// drag started). Stays fixed across the drag so the handle
        /// only moves up / down.
        anchor_xy: [f32; 2],
        /// World Z where the drag-start ray crossed the vertical line
        /// — subtracted from each `MouseMove`'s projection to get the
        /// per-frame delta. Without this anchor the gizmo would jump
        /// to wherever the mouse first lands when the camera angle
        /// is shallow.
        anchor_z: f32,
        start_matrix: [f32; 16],
    },
    /// Left-button down landed on one of the rotate gizmo's three
    /// per-axis corner handles (MatterCAD's `RotateCornerControl`).
    /// Each `MouseMove` intersects the cursor ray with the rotation
    /// plane (`normal = axis`, through `center`), measures the pointer
    /// angle in that plane, snaps it, and applies the rotation about
    /// the world `axis` through `center` to `start_matrix`. The
    /// rotation is pre-multiplied (applied on the LEFT of the node
    /// matrix) so the body spins about the world axis regardless of
    /// any upstream transform — see
    /// [`atomartist_lib::graph::node::rotate_about_world_axis`]. The
    /// matrix is always derived from `start_matrix` + the snapped
    /// delta, so a coalesced drag undoes back to `start_matrix`.
    RotateBodyAxis {
        node_id: NodeId,
        /// Which world axis the body spins about: 0=X, 1=Y, 2=Z.
        axis: u8,
        /// World point the rotation axis passes through — the
        /// selection's centre with the axis component moved to the
        /// control corner's plane. Fixed across the drag.
        center: [f32; 3],
        /// Pointer angle in the rotation plane at mouse-down. The
        /// snapped rotation each frame is relative to this anchor.
        anchor_angle: f32,
        /// Current snapped rotation (radians) from the anchor. Updated
        /// every `MouseMove`; read by the compass to draw the swept
        /// wedge + needle and by the degrees readout. Starts at 0.
        snapped: f32,
        /// World-space ring radius (centre→control distance) captured at
        /// mouse-down. Held fixed so the compass stays put while the
        /// body's AABB rotates under it, and reused as the base for the
        /// 8-point snap-mark radius.
        radius: f32,
        start_matrix: [f32; 16],
    },
}
