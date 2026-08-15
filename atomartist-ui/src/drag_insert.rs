//! Drag-insert gesture controller — dragging an item out of the
//! favorites bar (rail glyph, expanded row, or embedded browser entry)
//! and dropping it into the node canvas
//! (`docs/file-browser-design.md` §1.3, §2 drop-pipeline rows, §5 step
//! 6e).
//!
//! Descendant of NodeDesigner's `parts-bar-drag.js` and MatterCAD's
//! `ViewDragDropHandler` / `InsertionGroupObject3D`:
//!
//! 1. **Threshold** — a press on a draggable item only becomes a drag
//!    once the pointer travels [`DRAG_THRESHOLD`] px (the ancestor's
//!    `DRAG_THRESHOLD_PX = 4`). Below that the release is still a plain
//!    click, so bar rows keep opening projects and browser entries keep
//!    selecting.
//! 2. **Ghost** — while the pointer is outside the canvas a floating
//!    [`DragGhost`](crate::drag_insert_ghost) follows it.
//! 3. **Insert on canvas-enter** — crossing into the canvas turns a
//!    node-type payload into a *real* node at the cursor (ghost off);
//!    further motion moves it from a **base-position snapshot**, so the
//!    move math can never accumulate drift.
//! 4. **Leave = remove + re-ghost** — dragging back out deletes the
//!    node again and the ghost returns.
//! 5. **Commit is one undo step** — the live node is inserted straight
//!    into the graph with *no* undo entry; on release it is lifted back
//!    out and re-added through a single [`AddNodeCmd`], so the whole
//!    gesture undoes in one press of Ctrl+Z. Release outside the canvas
//!    (or Escape) leaves the undo stack untouched.
//!
//! # Two drop targets since 6f-4
//!
//! The node canvas is one; the **3-D viewport** is the other (design
//! §5b, step 6f-4 — "parts drag targets the 3-D viewport"). A drop on
//! the bed carries nothing live: v1 keeps the ghost over the viewport
//! all the way to the release (a live carry needs the drop-position
//! raycast that step defers), then inserts through
//! [`crate::node_insertion`] — placed left of the Output node and wired
//! into its first free input, both inside the gesture's single undo
//! step. Releasing over the viewport pane's *chrome* (the bar, its
//! handle) is still a cancel: the published viewport rectangle starts
//! where the bar ends.
//!
//! # File payloads drop on release
//!
//! A `.atmr` / `.stl` / `.obj` / `.3mf` payload cannot be carried live:
//! its bytes arrive through the storage job pump, so "insert on enter"
//! would need MatterCAD's async placeholder object. v1 therefore keeps
//! the ghost all the way to the release and then calls
//! [`AppState::import_dropped_file`] — *the same function the OS
//! file-drop handler calls* — whose own `AddNodeCmd` is the gesture's
//! single undo entry. The state machine below already has the
//! insert/remove hooks the live carry would use (see
//! [`DragInsert::update`]), so the placeholder can slot in later without
//! reshaping the controller.
//!
//! # Coordinates
//!
//! Every position handed to this controller is **favorites-bar-local**
//! and Y-up. Since step 6f-1 the bar is docked in the *3-D viewport*
//! pane while the drop target is the node canvas in the other pane of
//! the splitter, so the canvas is no longer a sibling whose left edge
//! can be inferred from the bar's width — it sits *below* the bar, at
//! negative bar-local `y`. The bar therefore publishes the canvas's
//! rectangle in its own coordinate space through
//! [`DragInsertHandle::set_canvas_rect`] (derived from the two
//! [`PaneRect`](crate::favorites_bar_host::PaneRect) probes), and that
//! rectangle is all the geometry [`DragInsert::in_canvas`] needs.
//! Canvas-space then follows from the editor's live pan / zoom, mirrored
//! onto [`AppState::canvas_pan`] / [`AppState::canvas_zoom`].

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use agg_gui::undo::UndoRedoCommand;
use agg_gui::{Point, Rect};
use atomartist_lib::graph::node::NodeId;
use atomartist_lib::graph::undo_commands::{AddNodeCmd, BatchCmd, ConnectToFreeInputCmd};
use atomartist_storage::StorageUri;

use crate::app_state::AppState;
use crate::drag_insert_ghost::DragGhost;
use crate::floating_overlay::FloatingOverlayHandle;
use crate::node_insertion;
use crate::storage_ops::NoticeLevel;

/// Pointer travel that turns a press into a drag. NodeDesigner's
/// `parts-bar-drag.js` `DRAG_THRESHOLD_PX`.
pub const DRAG_THRESHOLD: f64 = 4.0;

/// What a drag is carrying.
#[derive(Clone, Debug, PartialEq)]
pub enum DragPayload {
    /// A palette node type — carried live once inside the canvas.
    NodeType {
        type_id: String,
        label: String,
        glyph: char,
    },
    /// A project / mesh file — dropped through the import path on
    /// release.
    File {
        uri: StorageUri,
        label: String,
        glyph: char,
    },
}

impl DragPayload {
    fn glyph(&self) -> char {
        match self {
            DragPayload::NodeType { glyph, .. } | DragPayload::File { glyph, .. } => *glyph,
        }
    }
    fn label(&self) -> &str {
        match self {
            DragPayload::NodeType { label, .. } | DragPayload::File { label, .. } => label,
        }
    }
}

/// How a gesture finished — the caller (bar / browser) uses this to
/// decide whether its own click behaviour still applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GestureEnd {
    /// No gesture was in flight; the caller owns the event.
    None,
    /// Press and release without passing the threshold: a click.
    Click,
    /// Dropped inside the canvas — the insert / import has run.
    Dropped,
    /// Released outside the canvas, or cancelled: nothing inserted.
    Cancelled,
}

/// The live node a node-type drag is carrying, plus the snapshot the
/// move math works from (ancestor rule: never integrate deltas).
struct Live {
    id: NodeId,
    base_canvas: [f64; 2],
    base_pointer: Point,
}

struct Gesture {
    payload: DragPayload,
    press: Point,
    /// Past the threshold — i.e. a real drag rather than a click.
    started: bool,
    ghost: Option<Rc<Cell<bool>>>,
    live: Option<Live>,
}

/// Controller state. Shared through [`DragInsertHandle`]; UI-thread only
/// (it owns `Rc`s and hands `Box<dyn Widget>`s to the overlay), which is
/// why it is not parked on [`AppState`].
struct DragInsert {
    state: AppState,
    overlay: FloatingOverlayHandle,
    /// The node canvas's rectangle in **bar-local** coordinates,
    /// published every layout by the bar. See the module docs on
    /// coordinates. Zero-sized until the first layout, which reads as
    /// "no canvas yet" — the drop target is not guessed.
    canvas: Rect,
    /// The 3-D viewport's rectangle in **bar-local** coordinates, also
    /// published every layout by the bar (step 6f-4). Deliberately
    /// excludes the bar and its handle, so a release over the bar's own
    /// chrome cancels. Zero-sized until the first layout.
    viewport: Rect,
    gesture: Option<Gesture>,
}

/// Cheap-to-clone handle on the controller, shared by every drag source
/// (the bar itself and the browser it hosts).
#[derive(Clone)]
pub struct DragInsertHandle {
    inner: Rc<RefCell<DragInsert>>,
}

impl DragInsertHandle {
    pub fn new(state: AppState, overlay: FloatingOverlayHandle) -> Self {
        Self {
            inner: Rc::new(RefCell::new(DragInsert {
                state,
                overlay,
                canvas: Rect::default(),
                viewport: Rect::default(),
                gesture: None,
            })),
        }
    }

    /// Publish the node canvas's rectangle in bar-local coordinates —
    /// the drop target the boundary test and the canvas-space mapping
    /// both work from. Called from the bar's `layout`.
    pub fn set_canvas_rect(&self, canvas: Rect) {
        self.inner.borrow_mut().canvas = canvas;
    }

    /// Publish the 3-D viewport's rectangle in bar-local coordinates —
    /// the second drop target (step 6f-4). The bar passes the part of
    /// its pane that is *not* the bar, so its own chrome never reads as
    /// the bed.
    pub fn set_viewport_rect(&self, viewport: Rect) {
        self.inner.borrow_mut().viewport = viewport;
    }

    /// A press landed on a draggable item at bar-local `pos`. Does
    /// nothing visible yet — the gesture is only a *candidate* until the
    /// pointer moves past [`DRAG_THRESHOLD`].
    pub fn press(&self, payload: DragPayload, pos: Point) {
        self.inner.borrow_mut().press(payload, pos);
    }

    /// Pointer moved to bar-local `pos`. Returns `true` when a gesture
    /// is in flight and the caller should treat the event as consumed.
    pub fn pointer_move(&self, pos: Point) -> bool {
        self.inner.borrow_mut().pointer_move(pos)
    }

    /// Pointer released at bar-local `pos`.
    pub fn pointer_up(&self, pos: Point) -> GestureEnd {
        self.inner.borrow_mut().pointer_up(pos)
    }

    /// Escape (or any other abort): remove anything inserted, drop the
    /// ghost. Returns `true` if a gesture was actually cancelled.
    pub fn cancel(&self) -> bool {
        self.inner.borrow_mut().cancel()
    }

    /// A press is being tracked (drag or not-yet-drag).
    pub fn is_pressed(&self) -> bool {
        self.inner.borrow().gesture.is_some()
    }

    /// A real drag is in flight — the `dragging` property the bar
    /// reflects for the harness (design §6).
    pub fn is_dragging(&self) -> bool {
        self.inner
            .borrow()
            .gesture
            .as_ref()
            .is_some_and(|g| g.started)
    }

    /// A ghost is currently floating (drag in flight, pointer outside
    /// the canvas).
    pub fn ghost_active(&self) -> bool {
        self.inner
            .borrow()
            .gesture
            .as_ref()
            .is_some_and(|g| g.ghost.is_some())
    }

    /// The node the gesture is carrying inside the canvas, if any.
    pub fn carried_node(&self) -> Option<NodeId> {
        self.inner
            .borrow()
            .gesture
            .as_ref()
            .and_then(|g| g.live.as_ref().map(|l| l.id))
    }
}

impl DragInsert {
    // ── Geometry ────────────────────────────────────────────────────

    /// Is this bar-local point over the node canvas? The canvas lives in
    /// the splitter's other pane, so its rectangle is published rather
    /// than inferred (see the module docs).
    fn in_canvas(&self, pos: Point) -> bool {
        self.canvas.width > 0.0 && self.canvas.height > 0.0 && self.canvas.contains(pos)
    }

    /// Is this bar-local point over the 3-D viewport? The bar sits in
    /// the same pane, and the rectangle the bar publishes starts where
    /// the bar ends, so its rail / panel / handle are never "the bed".
    fn in_viewport(&self, pos: Point) -> bool {
        self.viewport.width > 0.0 && self.viewport.height > 0.0 && self.viewport.contains(pos)
    }

    /// Canvas-space centre of the node canvas — the position fallback
    /// when the graph has no Output node to place relative to. Zero when
    /// the canvas rectangle has not been published yet.
    fn canvas_center(&self) -> [f64; 2] {
        if self.canvas.width <= 0.0 || self.canvas.height <= 0.0 {
            return [0.0, 0.0];
        }
        self.canvas_pos(Point::new(
            self.canvas.x + self.canvas.width * 0.5,
            self.canvas.y + self.canvas.height * 0.5,
        ))
    }

    /// Bar-local → canvas-space, at the editor's live pan / zoom.
    fn canvas_pos(&self, pos: Point) -> [f64; 2] {
        let zoom = *self.state.canvas_zoom.lock().unwrap();
        let zoom = if zoom.is_finite() && zoom > 1e-6 {
            zoom
        } else {
            1.0
        };
        let pan = *self.state.canvas_pan.lock().unwrap();
        // Bar-local → canvas-widget-local: subtract the canvas's own
        // origin, which is below the bar since 6f-1 (negative `y`).
        let local_x = pos.x - self.canvas.x;
        let local_y = pos.y - self.canvas.y;
        [(local_x - pan[0]) / zoom, (local_y - pan[1]) / zoom]
    }

    fn zoom(&self) -> f64 {
        let zoom = *self.state.canvas_zoom.lock().unwrap();
        if zoom.is_finite() && zoom > 1e-6 {
            zoom
        } else {
            1.0
        }
    }

    // ── Gesture ─────────────────────────────────────────────────────

    fn press(&mut self, payload: DragPayload, pos: Point) {
        // A second press while a gesture is live must not orphan it.
        // agg-gui has one capture slot, so a right-button press
        // mid-drag re-targets it and the left MouseUp never comes back
        // here: without this the carried node would stay in the graph
        // owned by nobody and unreachable from the undo stack.
        self.cancel();
        self.gesture = Some(Gesture {
            payload,
            press: pos,
            started: false,
            ghost: None,
            live: None,
        });
    }

    fn pointer_move(&mut self, pos: Point) -> bool {
        let Some(mut gesture) = self.gesture.take() else {
            return false;
        };
        if !gesture.started {
            let dx = pos.x - gesture.press.x;
            let dy = pos.y - gesture.press.y;
            if dx * dx + dy * dy < DRAG_THRESHOLD * DRAG_THRESHOLD {
                self.gesture = Some(gesture);
                return true;
            }
            gesture.started = true;
            agg_gui::animation::request_draw();
        }
        self.update(&mut gesture, pos);
        self.gesture = Some(gesture);
        true
    }

    /// One step of the state machine for a started gesture: insert on
    /// canvas-enter, move while inside, remove + re-ghost on leave.
    fn update(&mut self, gesture: &mut Gesture, pos: Point) {
        let inside = self.in_canvas(pos);
        let carries_live = matches!(gesture.payload, DragPayload::NodeType { .. });
        if inside && carries_live {
            self.hide_ghost(gesture);
            if gesture.live.is_none() {
                self.insert_live(gesture, pos);
            } else {
                self.move_live(gesture, pos);
            }
            return;
        }
        if !inside {
            self.remove_live(gesture);
        }
        self.show_ghost(gesture);
    }

    fn pointer_up(&mut self, pos: Point) -> GestureEnd {
        let Some(mut gesture) = self.gesture.take() else {
            return GestureEnd::None;
        };
        let inside = self.in_canvas(pos);
        if !gesture.started {
            self.hide_ghost(&mut gesture);
            // Sub-threshold, but the release still has to have happened
            // where the press did. A press on a row followed by a
            // release over the canvas (a teleporting synthetic event, or
            // a pointer warp) is not a click on that row, and must not
            // activate it.
            return if inside && !self.in_canvas(gesture.press) {
                GestureEnd::Cancelled
            } else {
                GestureEnd::Click
            };
        }
        self.hide_ghost(&mut gesture);
        agg_gui::animation::request_draw();
        if !inside {
            if self.in_viewport(pos) {
                // Dropped on the bed (step 6f-4): nothing was carried
                // live there, so the insert happens now, positioned and
                // wired by the shared helper.
                self.remove_live(&mut gesture);
                return self.drop_on_viewport(&gesture);
            }
            // Released over the bar / chrome: nothing inserted, nothing
            // on the undo stack.
            self.remove_live(&mut gesture);
            return GestureEnd::Cancelled;
        }
        match &gesture.payload {
            DragPayload::NodeType { .. } => {
                if gesture.live.is_none() {
                    // Pointer jumped from bar to canvas in one step —
                    // no move ever reported the crossing.
                    self.insert_live(&mut gesture, pos);
                } else {
                    self.move_live(&mut gesture, pos);
                }
                self.commit_live(&mut gesture);
            }
            DragPayload::File { uri, label, .. } => {
                let canvas_pos = self.canvas_pos(pos);
                // Exactly the OS file-drop path (`top_level`'s handler
                // calls the same function), so a dragged browser entry
                // and a dragged OS file land identically.
                if !self.state.import_dropped_file(uri, canvas_pos) {
                    // No importer for this format: the drop did nothing,
                    // so say so rather than let it read as success.
                    self.state
                        .notify(NoticeLevel::Error, format!("Cannot import {label}"));
                    return GestureEnd::Cancelled;
                }
            }
        }
        GestureEnd::Dropped
    }

    fn cancel(&mut self) -> bool {
        let Some(mut gesture) = self.gesture.take() else {
            return false;
        };
        self.hide_ghost(&mut gesture);
        self.remove_live(&mut gesture);
        agg_gui::animation::request_draw();
        true
    }

    // ── Ghost ───────────────────────────────────────────────────────

    fn show_ghost(&mut self, gesture: &mut Gesture) {
        if gesture.ghost.is_some() {
            return;
        }
        let flag = Rc::new(Cell::new(false));
        let ghost = DragGhost::new(
            gesture.payload.glyph(),
            gesture.payload.label().to_string(),
            flag.clone(),
        );
        self.overlay.set(Box::new(ghost), flag.clone());
        gesture.ghost = Some(flag);
    }

    fn hide_ghost(&mut self, gesture: &mut Gesture) {
        if let Some(flag) = gesture.ghost.take() {
            // Two ways a ghost can be alive: already claimed by the
            // host (the close flag retires it) or still queued in the
            // handle's slot because the host was busy showing something
            // else (the colour picker). Retract covers the second —
            // otherwise the ghost would pop up, unowned, the next time
            // the host fell empty.
            flag.set(true);
            self.overlay.retract(&flag);
            agg_gui::animation::request_draw();
        }
    }

    // ── Live node ───────────────────────────────────────────────────

    /// Insert the carried node type straight into the active graph —
    /// deliberately *without* an undo entry, so the whole gesture can
    /// commit as one (see [`Self::commit_live`]).
    fn insert_live(&mut self, gesture: &mut Gesture, pos: Point) {
        let DragPayload::NodeType { type_id, .. } = &gesture.payload else {
            return;
        };
        let canvas = self.canvas_pos(pos);
        let graph = self.state.active_graph();
        let id = {
            let mut g = graph.lock().unwrap();
            crate::node_helpers::add_node_with_defaults(
                &mut g,
                &self.state.registry,
                type_id,
                canvas,
            )
        };
        let Some(id) = id else {
            return;
        };
        gesture.live = Some(Live {
            id,
            base_canvas: canvas,
            base_pointer: pos,
        });
        // Live eval — same trigger every other insert path uses, so the
        // 3-D view reacts while the node is still being carried.
        self.state.schedule_evaluate_after_edit();
        agg_gui::animation::request_draw();
    }

    /// Move the carried node. Position is always
    /// `base + (pointer - base_pointer) / zoom`, never an accumulated
    /// sum, so wiggling the cursor cannot drift the node.
    fn move_live(&mut self, gesture: &mut Gesture, pos: Point) {
        let Some(live) = gesture.live.as_ref() else {
            return;
        };
        let zoom = self.zoom();
        let next = [
            live.base_canvas[0] + (pos.x - live.base_pointer.x) / zoom,
            live.base_canvas[1] + (pos.y - live.base_pointer.y) / zoom,
        ];
        let graph = self.state.active_graph();
        let _ = graph.lock().unwrap().set_position(live.id, next);
        agg_gui::animation::request_draw();
    }

    /// Dragged back out: take the node away again (no undo entry, since
    /// none was ever pushed).
    fn remove_live(&mut self, gesture: &mut Gesture) {
        let Some(live) = gesture.live.take() else {
            return;
        };
        let graph = self.state.active_graph();
        let _ = graph.lock().unwrap().remove_node(live.id);
        self.state.schedule_evaluate_after_edit();
        agg_gui::animation::request_draw();
    }

    /// Turn the carried node into exactly one undo step: lift it back
    /// out of the graph and re-add it — together with its auto-wire, if
    /// one applies — through a single command. Net graph state is
    /// unchanged apart from the new noodle; net undo state is one entry.
    ///
    /// The node keeps **where the user dropped it**: a position the user
    /// chose is never overridden by the placement helper (design §5b,
    /// step 6f-4). Only the wiring is shared with the bed drop.
    fn commit_live(&mut self, gesture: &mut Gesture) {
        let Some(live) = gesture.live.take() else {
            return;
        };
        self.lift_into_one_undo_step(live.id);
    }

    /// A drop on the 3-D viewport. Nothing was carried live there, so
    /// this is the whole insert: place through
    /// [`crate::node_insertion`], wire to Output, one undo step.
    fn drop_on_viewport(&mut self, gesture: &Gesture) -> GestureEnd {
        match &gesture.payload {
            DragPayload::NodeType { type_id, .. } => {
                let graph = self.state.active_graph();
                let id = {
                    let mut g = graph.lock().unwrap();
                    crate::node_helpers::add_node_with_defaults(
                        &mut g,
                        &self.state.registry,
                        type_id,
                        [0.0, 0.0],
                    )
                };
                let Some(id) = id else {
                    return GestureEnd::Cancelled;
                };
                let center = self.canvas_center();
                {
                    let mut g = graph.lock().unwrap();
                    node_insertion::place_inserted_node(&mut g, &self.state.registry, id, center);
                }
                self.lift_into_one_undo_step(id);
                GestureEnd::Dropped
            }
            DragPayload::File { uri, label, .. } => {
                // The import spawns its node asynchronously, so it gets
                // the helper's position for a node that does not exist
                // yet — the ancestor's `[200, 100]` default size.
                //
                // Resolved against **the root graph**, because that is
                // where `import_dropped_file` inserts: while the user is
                // drilled into a component the active graph is a
                // different one, and placing against its Output would
                // put the import somewhere unrelated to where it lands.
                let pos = {
                    let g = self.state.graph.lock().unwrap();
                    node_insertion::position_for_insertion(
                        &g,
                        &self.state.registry,
                        node_insertion::DEFAULT_NODE_SIZE,
                        None,
                        self.canvas_center(),
                    )
                };
                if !self.state.import_dropped_file(uri, pos) {
                    self.state
                        .notify(NoticeLevel::Error, format!("Cannot import {label}"));
                    return GestureEnd::Cancelled;
                }
                GestureEnd::Dropped
            }
        }
    }

    /// Lift `id` out of the graph and re-add it — plus its auto-wire, if
    /// [`node_insertion::plan_auto_connect`] finds one — as a single
    /// undo entry. The node was inserted with no undo entry of its own
    /// (that is what makes the whole gesture undo in one step), so this
    /// is the only thing the undo stack ever sees for it.
    fn lift_into_one_undo_step(&mut self, id: NodeId) {
        let graph = self.state.active_graph();
        // Plan the wire *before* the lift, while the node's own sockets
        // are still readable. The plan carries the source endpoint and
        // the Output *node* only — the Output's target slot is re-
        // resolved by the command on every do / redo, because
        // disconnecting deletes that slot and regrows it under a new uid
        // (see `AutoWirePlan`).
        let plan = {
            let g = graph.lock().unwrap();
            node_insertion::plan_auto_connect(&g, id)
        };
        let node = {
            let mut g = graph.lock().unwrap();
            g.remove_node(id).ok().map(|(node, _detached)| node)
        };
        let Some(node) = node else {
            return;
        };
        let add = AddNodeCmd::new(graph.clone(), node);
        let cmd: Box<dyn UndoRedoCommand> = match plan {
            Some(plan) => Box::new(BatchCmd::new(
                "Add Node",
                vec![
                    Box::new(add),
                    Box::new(ConnectToFreeInputCmd::new(
                        graph,
                        self.state.registry.clone(),
                        plan.from,
                        plan.from_socket,
                        plan.output,
                    )),
                ],
            )),
            None => Box::new(add),
        };
        self.state.active_undo().lock().unwrap().add_and_do(cmd);
        self.state.schedule_evaluate_after_edit();
        agg_gui::animation::request_draw();
    }
}

#[cfg(test)]
#[path = "drag_insert_tests.rs"]
mod tests;
