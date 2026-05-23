//! App-level host for floating editor dialogs.
//!
//! Designed for one use case today: the `ColorWheelPicker` dialog
//! that opens when the user clicks a color row inside a node's
//! property panel. Historically the node-editor stored the dialog as
//! its own `overlay` field, which meant the user could only drag the
//! picker inside the node-editor's pane — try to drag past the
//! splitter into the 3-D viewport and the dialog clipped against the
//! pane boundary.
//!
//! ## Reparenting to the app shell
//!
//! Moving the dialog up the widget tree so it lives at the top of the
//! main window's `Stack` (alongside the debug windows) gives the
//! dialog the **entire window** as its coordinate space. The
//! draggable `Window` chrome that wraps the picker can then position
//! the dialog anywhere on screen — no clipping, no synthetic bounds
//! clamping by the pane that opened it.
//!
//! ## Cross-crate channel
//!
//! The node-editor still constructs the dialog (it owns the model
//! callbacks for live preview / commit / cancel). It hands the
//! finished `(Box<dyn Widget>, close_flag)` pair off through the
//! [`agg_gui_node_editor::NodeEditor::with_overlay_sink`] callback —
//! which AtomArtist's app shell wires to a [`FloatingOverlayHandle`]
//! shared with the [`FloatingOverlayHost`] widget. The host pulls
//! the dialog into its `children[0]` slot on the next layout pass.
//!
//! ## Why a separate slot rather than just calling `Stack::add`
//!
//! The top-level `Stack` is built once during `build_app`; its
//! `children` Vec is private to the `Stack` impl. The handle pattern
//! lets the dialog appear and disappear during runtime without
//! mutating the `Stack`'s contents — only the host's single-element
//! `children` Vec swings between empty and one entry.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use agg_gui::{
    DrawCtx, Event, EventResult, HAnchor, Insets, Point, Rect, Size, VAnchor, Widget, WidgetBase,
};

/// Shared spawn-slot for a floating dialog and its self-close flag.
///
/// Cheap to clone (it's an `Rc` internally). Lives on the UI thread
/// only — never crosses the eval thread, so `Rc` + `RefCell` is the
/// right choice (vs `Arc<Mutex<>>`) and is also what lets us hold
/// `Box<dyn Widget>` (which contains `Rc`s of its own and cannot be
/// `Send`).
#[derive(Clone)]
pub struct FloatingOverlayHandle {
    slot: Rc<RefCell<Option<PendingOverlay>>>,
}

struct PendingOverlay {
    widget: Box<dyn Widget>,
    close_flag: Rc<Cell<bool>>,
}

impl FloatingOverlayHandle {
    pub fn new() -> Self {
        Self {
            slot: Rc::new(RefCell::new(None)),
        }
    }

    /// Drop the currently-pending overlay (if the host hasn't claimed
    /// it yet) and install a fresh one. Requests a redraw so the host
    /// picks it up on the next frame.
    pub fn set(&self, widget: Box<dyn Widget>, close_flag: Rc<Cell<bool>>) {
        *self.slot.borrow_mut() = Some(PendingOverlay { widget, close_flag });
        // No need to bump the invalidation epoch — the host's layout
        // walk this frame will sync.
        agg_gui::animation::request_draw();
    }

    /// Internal: called by [`FloatingOverlayHost::sync_from_handle`].
    fn take(&self) -> Option<(Box<dyn Widget>, Rc<Cell<bool>>)> {
        self.slot
            .borrow_mut()
            .take()
            .map(|p| (p.widget, p.close_flag))
    }

    /// `true` while a dialog is queued (the host has not yet pulled
    /// it). Mostly useful for tests / asserts.
    pub fn is_pending(&self) -> bool {
        self.slot.borrow().is_some()
    }
}

impl Default for FloatingOverlayHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// Transparent screen-filling widget that owns whatever floating
/// dialog is currently active.
///
/// Place this last in the app's top-level `Stack` (after the main
/// column and after the debug windows): paint order makes the dialog
/// the topmost layer, and hit-test reverse order makes it the first
/// to claim mouse events when its bounds cover the click.
///
/// When no dialog is active the host's `hit_test` returns `false` so
/// every event falls through to the underlying widgets (the menu
/// bar, viewport, node-editor pane, etc.).
pub struct FloatingOverlayHost {
    handle: FloatingOverlayHandle,
    /// Mirror of the close flag for the dialog currently held in
    /// `children[0]`. `None` when no dialog is active. Drained on
    /// every layout + event pass; firing replaces `children[0]` with
    /// nothing.
    active_close_flag: Option<Rc<Cell<bool>>>,
    children: Vec<Box<dyn Widget>>,
    bounds: Rect,
    base: WidgetBase,
}

impl FloatingOverlayHost {
    pub fn new(handle: FloatingOverlayHandle) -> Self {
        Self {
            handle,
            active_close_flag: None,
            children: Vec::new(),
            bounds: Rect::default(),
            base: WidgetBase::new()
                .with_h_anchor(HAnchor::STRETCH)
                .with_v_anchor(VAnchor::STRETCH),
        }
    }

    /// Borrow the underlying handle. Useful for test setup that
    /// wants to populate the slot directly.
    pub fn handle(&self) -> FloatingOverlayHandle {
        self.handle.clone()
    }

    /// Sync the host's `children` Vec against the shared handle:
    /// drain the active dialog's close flag (closing it if fired)
    /// and then claim any newly-queued dialog from the handle.
    ///
    /// Called at the start of every `layout`, `paint`, and `on_event`
    /// so the host stays consistent regardless of which pass picks
    /// up the state change first.
    fn sync_from_handle(&mut self) {
        // Step 1: drop the current dialog if it asked to close.
        if let Some(flag) = &self.active_close_flag {
            if flag.replace(false) {
                self.children.clear();
                self.active_close_flag = None;
                agg_gui::animation::request_draw();
            }
        }
        // Step 2: claim a newly-queued dialog if we don't already
        // have one. (We deliberately don't replace an active dialog
        // with a fresh one — the close flag is the only way for the
        // current dialog to make way for the next.)
        if self.children.is_empty() {
            if let Some((widget, close_flag)) = self.handle.take() {
                self.children.push(widget);
                self.active_close_flag = Some(close_flag);
                agg_gui::animation::request_draw();
            }
        }
    }
}

impl Widget for FloatingOverlayHost {
    fn type_name(&self) -> &'static str {
        "FloatingOverlayHost"
    }
    fn bounds(&self) -> Rect {
        self.bounds
    }
    fn set_bounds(&mut self, b: Rect) {
        self.bounds = b;
    }
    fn children(&self) -> &[Box<dyn Widget>] {
        &self.children
    }
    fn children_mut(&mut self) -> &mut Vec<Box<dyn Widget>> {
        &mut self.children
    }

    fn margin(&self) -> Insets {
        Insets::ZERO
    }
    fn h_anchor(&self) -> HAnchor {
        self.base.h_anchor
    }
    fn v_anchor(&self) -> VAnchor {
        self.base.v_anchor
    }
    fn widget_base(&self) -> Option<&WidgetBase> {
        Some(&self.base)
    }
    fn widget_base_mut(&mut self) -> Option<&mut WidgetBase> {
        Some(&mut self.base)
    }

    fn layout(&mut self, available: Size) -> Size {
        self.sync_from_handle();
        if let Some(child) = self.children.first_mut() {
            let desired = child.layout(available);
            // If the dialog already has a remembered position (the
            // user dragged it last frame, or saved bounds from a
            // previous session), respect it. Otherwise pick a
            // centred-near-the-top initial placement so the picker
            // doesn't cover the node the user just clicked on.
            let current = child.bounds();
            let has_pos = current.width > 0.0 && current.height > 0.0;
            if has_pos {
                child.set_bounds(current);
            } else {
                let w = desired.width.min(available.width).max(0.0);
                let h = desired.height.min(available.height).max(0.0);
                let x = ((available.width - w) * 0.5).max(0.0);
                // 60px below the top in Y-up coords means y =
                // available.height - h - 60 (the dialog's bottom-left
                // origin sits below the top edge of the window).
                let y = ((available.height - h) - 60.0).max(0.0);
                child.set_bounds(Rect::new(x, y, w, h));
            }
        }
        available
    }

    fn paint(&mut self, _ctx: &mut dyn DrawCtx) {
        // The host renders nothing of its own — paint_subtree
        // recurses into our child (the dialog) which draws its
        // window chrome + picker content. Re-syncing here is
        // belt-and-suspenders for the layout-skipped case (debug
        // window paths that paint without re-laying out can still
        // pick up a new dialog).
        self.sync_from_handle();
    }

    /// Only claim hits that land inside the dialog's own bounds. When
    /// the host has no dialog, returns `false` so events fall through
    /// to the rest of the `Stack` siblings (the menu bar, viewport,
    /// canvas, etc.). When a dialog is active, the framework will
    /// then ask the child (`Window`) whether it claims the position
    /// — the child's own hit_test makes the precise call.
    fn hit_test(&self, local_pos: Point) -> bool {
        if let Some(child) = self.children.first() {
            let b = child.bounds();
            local_pos.x >= b.x
                && local_pos.x <= b.x + b.width
                && local_pos.y >= b.y
                && local_pos.y <= b.y + b.height
        } else {
            false
        }
    }

    fn on_event(&mut self, _event: &Event) -> EventResult {
        // Drain the close flag once per event — covers the case
        // where the dialog's Select / Cancel button fired during
        // this event's dispatch and we need to drop the dialog on
        // the same frame (rather than wait for next layout).
        self.sync_from_handle();
        EventResult::Ignored
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agg_gui::Point;

    /// A trivial widget used as a stand-in for the picker dialog.
    /// Reports a fixed bounds so we can drive hit-tests against
    /// known coordinates.
    struct DummyDialog {
        bounds: Rect,
        children: Vec<Box<dyn Widget>>,
    }
    impl Widget for DummyDialog {
        fn type_name(&self) -> &'static str {
            "DummyDialog"
        }
        fn bounds(&self) -> Rect {
            self.bounds
        }
        fn set_bounds(&mut self, b: Rect) {
            self.bounds = b;
        }
        fn children(&self) -> &[Box<dyn Widget>] {
            &self.children
        }
        fn children_mut(&mut self) -> &mut Vec<Box<dyn Widget>> {
            &mut self.children
        }
        fn layout(&mut self, _available: Size) -> Size {
            Size::new(200.0, 150.0)
        }
        fn paint(&mut self, _ctx: &mut dyn DrawCtx) {}
        fn on_event(&mut self, _event: &Event) -> EventResult {
            EventResult::Ignored
        }
    }

    fn dialog_at(b: Rect) -> Box<dyn Widget> {
        Box::new(DummyDialog {
            bounds: b,
            children: Vec::new(),
        })
    }

    /// Empty host reports `is_pending = false` and refuses every hit.
    #[test]
    fn empty_host_passes_through_events() {
        let handle = FloatingOverlayHandle::new();
        let host = FloatingOverlayHost::new(handle.clone());
        assert!(!handle.is_pending());
        // Any local position should miss when there's no dialog.
        assert!(!host.hit_test(Point::new(0.0, 0.0)));
        assert!(!host.hit_test(Point::new(500.0, 500.0)));
    }

    /// Spawning a dialog through the handle queues it; the next
    /// layout pulls it into the host's children Vec.
    #[test]
    fn layout_claims_pending_dialog_from_handle() {
        let handle = FloatingOverlayHandle::new();
        let mut host = FloatingOverlayHost::new(handle.clone());
        let close_flag = Rc::new(Cell::new(false));
        handle.set(dialog_at(Rect::new(0.0, 0.0, 0.0, 0.0)), close_flag.clone());
        assert!(handle.is_pending());

        let _ = host.layout(Size::new(800.0, 600.0));

        assert!(!handle.is_pending(), "host should have taken the dialog");
        assert_eq!(host.children().len(), 1);
        // Empty initial bounds → host centred + nudged down from top.
        let child_b = host.children()[0].bounds();
        assert!(child_b.width > 0.0 && child_b.height > 0.0);
    }

    /// A dialog that arrives with non-empty bounds keeps them — this
    /// is how the `Window` widget preserves drag position across
    /// layout passes.
    #[test]
    fn layout_preserves_dialog_initial_bounds() {
        let handle = FloatingOverlayHandle::new();
        let mut host = FloatingOverlayHost::new(handle.clone());
        let close_flag = Rc::new(Cell::new(false));
        let preset = Rect::new(123.0, 45.0, 200.0, 150.0);
        handle.set(dialog_at(preset), close_flag);

        let _ = host.layout(Size::new(800.0, 600.0));

        let child_b = host.children()[0].bounds();
        assert_eq!(child_b.x, 123.0);
        assert_eq!(child_b.y, 45.0);
    }

    /// The close flag fires → next layout drops the child.
    #[test]
    fn close_flag_drops_child_on_next_layout() {
        let handle = FloatingOverlayHandle::new();
        let mut host = FloatingOverlayHost::new(handle.clone());
        let close_flag = Rc::new(Cell::new(false));
        handle.set(
            dialog_at(Rect::new(0.0, 0.0, 200.0, 150.0)),
            close_flag.clone(),
        );

        let _ = host.layout(Size::new(800.0, 600.0));
        assert_eq!(host.children().len(), 1);

        close_flag.set(true);
        let _ = host.layout(Size::new(800.0, 600.0));
        assert_eq!(
            host.children().len(),
            0,
            "host should have dropped the closed dialog"
        );
    }

    /// Hit-test only claims positions inside the dialog's bounds.
    #[test]
    fn hit_test_gates_by_child_bounds() {
        let handle = FloatingOverlayHandle::new();
        let mut host = FloatingOverlayHost::new(handle.clone());
        let close_flag = Rc::new(Cell::new(false));
        let dialog_b = Rect::new(100.0, 100.0, 200.0, 150.0);
        handle.set(dialog_at(dialog_b), close_flag);

        let _ = host.layout(Size::new(800.0, 600.0));

        // Inside the dialog → claim.
        assert!(host.hit_test(Point::new(150.0, 150.0)));
        // Just outside → reject so events fall through to siblings.
        assert!(!host.hit_test(Point::new(50.0, 150.0)));
        assert!(!host.hit_test(Point::new(150.0, 50.0)));
        assert!(!host.hit_test(Point::new(400.0, 150.0)));
    }
}
