//! Input routing for [`super::FileBrowser`] — every press and every key
//! the browser answers.
//!
//! A child module of `widget.rs` (declared there with `#[path]`), so it
//! reads the widget's private fields directly while keeping the widget
//! file itself assembly + `Widget` impl. `widget_geom` decides *where*
//! the chrome is; this decides what happens when it is hit.
//!
//! # Keyboard: the modal face only
//!
//! The embedded face (the favorites bar) must never grab keys — it lives
//! in the ordinary widget tree beside the node canvas, so a browser that
//! answered Alt+Left or Escape from there would steal them from whatever
//! the user is actually working in. That is NodeDesigner's
//! `mountEmbedded()` contract, and [`super::FileBrowser::takes_keys`]
//! enforces it: the modal faces ([`BrowserMode::Open`] /
//! [`BrowserMode::Save`]) answer keys, the embedded face is inert and the
//! bar's clear button / Back button still work by mouse in both.
//!
//! Two channels, because agg-gui has two:
//!
//! - `Event::KeyDown` reaches us by *bubbling* out of a focused child —
//!   the search field ignoring Escape is what lets Escape clear the
//!   search rather than close the dialog.
//! - `on_unconsumed_key` is the whole-tree fallback for a key nothing
//!   focused wanted, which is how Alt+Left works with focus nowhere.
//!   Escape is deliberately *not* handled there: with nothing focused,
//!   Escape belongs to the modal sheet, which closes on it.

use agg_gui::{EventResult, Key, Modifiers, Point};

use super::{BrowserMode, FileBrowser};

impl FileBrowser {
    /// Whether this face answers keyboard input at all (see the module
    /// docs).
    pub(super) fn takes_keys(&self) -> bool {
        !matches!(self.mode, BrowserMode::Embedded)
    }

    /// Empty the search box — both the filter on the model and the text
    /// in the bound field, which picks the cell up on its next layout.
    /// Reports whether anything was there to clear.
    pub(super) fn clear_search(&mut self) -> bool {
        if self.model.search().is_empty() {
            return false;
        }
        self.model.set_search("");
        self.search_cell.borrow_mut().clear();
        agg_gui::animation::request_draw();
        true
    }

    /// Walk one step back down the navigation history.
    pub(super) fn go_back(&mut self) -> bool {
        if !self.model.back(&self.state) {
            return false;
        }
        self.scroll = 0.0;
        agg_gui::animation::request_draw();
        true
    }

    /// A key that reached us — either bubbled out of the focused search
    /// field or offered by the whole-tree fallback. `focused` says which,
    /// because Escape is only ours in the first case.
    pub(super) fn handle_key(&mut self, key: &Key, mods: Modifiers, focused: bool) -> EventResult {
        if !self.takes_keys() {
            return EventResult::Ignored;
        }
        match key {
            // ND's Alt+Left. `alt` and nothing else, so Ctrl+Alt+Left
            // (a window-manager gesture on several desktops) is not us.
            Key::ArrowLeft if mods.alt && !mods.ctrl && !mods.meta && !mods.shift => {
                if self.go_back() {
                    EventResult::Consumed
                } else {
                    EventResult::Ignored
                }
            }
            Key::Escape if focused => {
                if self.clear_search() {
                    EventResult::Consumed
                } else {
                    // Nothing to clear: let it through to the sheet, so
                    // Escape still cancels the dialog.
                    EventResult::Ignored
                }
            }
            _ => EventResult::Ignored,
        }
    }

    /// Left press, in widget-local coordinates.
    pub(super) fn on_mouse_down(&mut self, pos: Point) -> EventResult {
        let clicks = self.clicks.register(pos);

        if let Some(index) = self
            .frame
            .sidebar_rows
            .iter()
            .position(|row| row.contains(pos))
        {
            let root = self.frame.roots[index].root.clone();
            self.model.navigate_to(&self.state, root);
            self.scroll = 0.0;
            return EventResult::Consumed;
        }

        // The clear button only exists while there is a search to clear,
        // in both faces (a bar with no keyboard still needs a way out of
        // a filter).
        if !self.model.search().is_empty() && self.frame.layout.search_clear.contains(pos) {
            self.clear_search();
            return EventResult::Consumed;
        }

        if self.frame.layout.back.contains(pos) {
            // A disabled Back still swallows its own press: the button is
            // painted there, and letting the click fall through to the
            // grid behind it would clear the selection.
            self.go_back();
            return EventResult::Consumed;
        }

        if self.frame.layout.crumbs.contains(pos) {
            if let Some(index) = self
                .frame
                .crumb_rects
                .iter()
                .position(|rect| rect.contains(pos))
            {
                let uri = self.frame.crumbs[index].uri.clone();
                self.model.navigate_to(&self.state, uri);
                self.scroll = 0.0;
                return EventResult::Consumed;
            }
            return EventResult::Consumed;
        }

        if let Some(index) = self.entry_at(pos) {
            let entry = self.frame.entries[index].clone();
            self.select(&entry);
            // Exactly two, not "two or more": the tracker counts
            // 1, 2, 3, 1, … within its window, so `>= 2` would let a
            // third rapid press activate a second time — a double-click
            // into a folder plus one more tap would land two levels
            // deep, and a file would be handed to the host twice.
            if clicks == 2 {
                self.activate(&entry);
            }
            // Selection stays on the press (both ancestors do that);
            // the drag is only a *candidate* until the pointer passes
            // the threshold, so a plain click is unaffected.
            if let (Some(insert), Some(payload)) = (self.insert.clone(), self.drag_payload(&entry))
            {
                insert.press(payload, self.to_parent(pos));
            }
            return EventResult::Consumed;
        }

        // Empty space inside the grid — including the gap between two
        // cards — clears the selection, the way both ancestors' file
        // panes do.
        if self.frame.layout.grid.contains(pos) {
            self.model.select(None);
            return EventResult::Consumed;
        }
        EventResult::Ignored
    }
}
