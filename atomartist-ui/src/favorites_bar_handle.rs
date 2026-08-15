//! The favorites bar's handle gesture: the 16 × 56 grip that is both a
//! toggle and a resize grip (`docs/file-browser-design.md` §2 "handle =
//! toggle **and** resize grip", step 6f-1).
//!
//! Split out of `favorites_bar.rs` (state + assembly) so the ancestor's
//! three numbers — the 3 px toggle-vs-resize threshold, the 120 px
//! collapse threshold, the 240 px minimum panel — live in one place with
//! unit tests that need no widget, no font and no frame.
//!
//! # The two thresholds are not the same thing
//!
//! [`DRAG_THRESHOLD`](crate::favorites_bar::DRAG_THRESHOLD) decides
//! whether the *gesture* is a click or a drag.
//! [`COLLAPSE_THRESHOLD_W`](crate::favorites_bar::COLLAPSE_THRESHOLD_W)
//! decides whether the *result* is an open panel or a closed one. A drag
//! that ends narrow snaps closed; a click toggles whatever the panel
//! currently is. Conflating them is what makes "snap closed keeps the
//! stored width" come out as "keeps ≈120 px".
//!
//! Because the bar is docked left, a rightward drag widens the panel, so
//! the raw width is simply `start + (x − press_x)`, never negated.

use crate::favorites_bar::{COLLAPSE_THRESHOLD_W, DRAG_THRESHOLD, MIN_EXPANDED_W};

/// An in-flight handle gesture. Starts as a possible toggle and becomes a
/// resize once the pointer has travelled past
/// [`DRAG_THRESHOLD`](crate::favorites_bar::DRAG_THRESHOLD).
pub(crate) struct HandleGesture {
    /// Bar-local x of the press. The bar's origin is pinned to the pane's
    /// left edge, so this stays comparable as the panel resizes.
    press_x: f64,
    /// Panel width when the gesture began (0 while collapsed).
    start_panel: f64,
    /// Raw pointer-derived panel width — deliberately *not* clamped,
    /// because the snap-closed decision is made against the raw value.
    raw: f64,
    moved: bool,
}

impl HandleGesture {
    pub(crate) fn begin(press_x: f64, panel_w: f64) -> Self {
        HandleGesture {
            press_x,
            start_panel: panel_w,
            raw: panel_w,
            moved: false,
        }
    }

    /// Feed a pointer x. Returns `true` once the gesture is a resize —
    /// until then the caller has nothing to redraw, and a release is
    /// still the toggle.
    pub(crate) fn pointer_x(&mut self, x: f64) -> bool {
        let dx = x - self.press_x;
        if !self.moved {
            if dx.abs() <= DRAG_THRESHOLD {
                return false;
            }
            self.moved = true;
        }
        self.raw = (self.start_panel + dx).max(0.0);
        true
    }

    /// Raw (unclamped) panel width the pointer is asking for.
    pub(crate) fn raw(&self) -> f64 {
        self.raw
    }

    /// Has this gesture passed the threshold, i.e. is it a resize rather
    /// than a pending toggle?
    pub(crate) fn is_resizing(&self) -> bool {
        self.moved
    }

    /// Would this gesture leave the panel open? Both the pull-open
    /// during the drag and the commit on release ask this.
    pub(crate) fn wants_open(&self) -> bool {
        self.raw >= COLLAPSE_THRESHOLD_W as f64
    }
}

/// Turn a raw, pointer-derived panel width into the width the panel will
/// actually be shown at: below the collapse threshold the panel is not
/// there at all, otherwise it is at least [`MIN_EXPANDED_W`] and at most
/// `max` (what the host pane allows).
pub(crate) fn clamp_panel(raw: f64, max: f64) -> f64 {
    if raw < COLLAPSE_THRESHOLD_W as f64 {
        0.0
    } else {
        raw.max(MIN_EXPANDED_W as f64).min(max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Under the 3 px threshold nothing happens at all — the release is
    /// still the toggle.
    #[test]
    fn a_short_press_is_not_a_resize() {
        let mut g = HandleGesture::begin(100.0, 0.0);
        assert!(!g.pointer_x(102.0), "2 px is not a drag");
        assert!(!g.pointer_x(97.5), "nor is 2.5 px the other way");
        assert!(!g.is_resizing());
        assert_eq!(g.raw(), 0.0, "and the width has not moved");
    }

    /// Past it, the panel follows the pointer — rightward widens,
    /// because the bar is docked left.
    #[test]
    fn dragging_right_widens_from_the_starting_width() {
        let mut g = HandleGesture::begin(100.0, 300.0);
        assert!(g.pointer_x(420.0));
        assert!(g.is_resizing());
        assert_eq!(g.raw(), 620.0);
        // And back the other way, from the same base — never integrated.
        assert!(g.pointer_x(60.0));
        assert_eq!(g.raw(), 260.0);
        // A pointer dragged past the bar's own left edge floors at zero.
        assert!(g.pointer_x(-500.0));
        assert_eq!(g.raw(), 0.0);
    }

    /// Once a gesture is a resize it stays one, even if the pointer
    /// wanders back inside the threshold.
    #[test]
    fn a_resize_does_not_turn_back_into_a_click() {
        let mut g = HandleGesture::begin(100.0, 200.0);
        assert!(g.pointer_x(140.0));
        assert!(g.pointer_x(101.0), "back near the press, still a resize");
        assert!(g.is_resizing());
        assert_eq!(g.raw(), 201.0);
    }

    /// The collapse threshold is 120, the minimum panel 240: a width
    /// between them opens at the minimum rather than as a sliver, and
    /// below it the panel is closed.
    #[test]
    fn clamping_follows_the_ancestors_two_numbers() {
        let max = 900.0;
        assert_eq!(clamp_panel(0.0, max), 0.0);
        assert_eq!(clamp_panel(119.0, max), 0.0, "below 120 = closed");
        assert_eq!(
            clamp_panel(120.0, max),
            MIN_EXPANDED_W as f64,
            "at the threshold the panel opens at its minimum"
        );
        assert_eq!(clamp_panel(300.0, max), 300.0);
        assert_eq!(clamp_panel(5000.0, max), max, "and never exceeds the pane");
    }

    /// `wants_open` is the release-time question, asked of the raw width
    /// so a wide-then-narrow drag closes.
    #[test]
    fn wants_open_tracks_the_raw_width() {
        let mut g = HandleGesture::begin(0.0, 0.0);
        assert!(!g.wants_open());
        g.pointer_x(300.0);
        assert!(g.wants_open());
        g.pointer_x(40.0);
        assert!(!g.wants_open(), "swept back below 120 on the way out");
    }
}
