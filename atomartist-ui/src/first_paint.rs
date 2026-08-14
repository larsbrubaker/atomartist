//! First-paint gate — guarantees a reactive host draws its first frame.
//!
//! Shared shell support (like [`crate::shell_init`]) rather than platform
//! glue, so the logic is reachable from native tests even though the only
//! consumer today is the wasm shell (`demo-wasm/src/lib.rs`, which is
//! `#![cfg(target_arch = "wasm32")]` and therefore untestable natively).
//!
//! Why this exists: the app defaults to [`agg_gui::RunMode::Reactive`]
//! (see `debug_windows.rs`), and a reactive host only paints when
//! `agg_gui::animation::wants_draw()` or `App::wants_draw()` says so.
//! On native, winit hands the shell an initial `RedrawRequested` event,
//! so the first frame is guaranteed. The browser's
//! `requestAnimationFrame` loop has no equivalent: nothing guarantees a
//! draw request is still pending at the moment async wgpu init resolves,
//! so — depending on load timing — the shell could skip its first frame
//! forever and the page stayed blank until a resize (whose own code path
//! bypasses the paint gate). This gate makes that first frame
//! unconditional and self-healing: it keeps forcing a paint until one
//! frame has actually been presented.

use std::cell::Cell;

/// One-shot latch: forces painting until the first frame is presented.
///
/// Hosts hold one of these (thread-local on wasm, since the browser main
/// thread owns the whole shell) and call [`Self::should_paint_tick`] in
/// place of a bare `wants_draw()` check, then [`Self::mark_painted`]
/// only after a frame has been acquired, painted, and presented — a tick
/// that bails out early (no surface texture available yet, for instance)
/// must leave the latch unset so the next tick tries again.
#[derive(Debug, Default)]
pub struct FirstPaintGate {
    painted: Cell<bool>,
}

impl FirstPaintGate {
    /// A gate that has not yet seen a presented frame.
    pub const fn new() -> Self {
        Self { painted: Cell::new(false) }
    }

    /// True once [`Self::mark_painted`] has run at least once.
    pub fn has_painted(&self) -> bool {
        self.painted.get()
    }

    /// Record that a frame was fully acquired, painted, and presented.
    pub fn mark_painted(&self) {
        self.painted.set(true);
    }

    /// Whether this host tick should paint.
    ///
    /// `host_wants_draw` is the host's normal reactive predicate
    /// (`animation::wants_draw() || app.wants_draw() || a due deadline`).
    /// It is taken lazily so a forced paint neither evaluates it
    /// needlessly nor promotes a due `request_draw_after` deadline into an
    /// immediate draw request early. Laziness is a nice-to-have here, not
    /// a correctness requirement: agg-gui's `wants_draw()` never clears
    /// the immediate flag, so consuming it would not lose a repaint.
    pub fn should_paint_tick(
        &self,
        resized: bool,
        host_wants_draw: impl FnOnce() -> bool,
    ) -> bool {
        !self.painted.get() || resized || host_wants_draw()
    }
}
