//! The frame body the shell's `paint` hook runs, plus the opt-in
//! frame-time breakdown logger.
//!
//! `agg-gui-shell` owns surface acquire, `begin_frame` / `end_frame` and
//! present; what is left here is AtomArtist's inspector wiring around
//! layout + paint (View → Debug → Inspector), and the
//! `ATOMARTIST_FRAME_LOG=1` per-stage cost attribution. Compared with the
//! pre-shell logger, the acquire and present spans are no longer
//! measurable from app code (the shell owns them); the whole-frame
//! wall-clock cost still is — the shell reports it as `Frame::duration`,
//! which is also what the Performance graph plots.

use agg_gui::App;
use agg_gui_shell::{Frame, WgpuGfxCtx};
use atomartist_ui::DebugWindowHandles;

// Per-frame inspector epoch tracker. Mirrors agg-gui's
// `render_app_frame` so the inspector tree only gets re-collected when
// widget invalidation actually changes — collecting every frame would
// torch the budget on a large widget tree.
thread_local! {
    static INSPECTOR_SNAPSHOT_EPOCH: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
}

/// Render one frame's contents: drain inspector edits, refresh the
/// inspector snapshot, lay out (when needed) and paint. Runs between the
/// shell's `begin_frame` and `end_frame`.
pub fn paint_app_frame(
    app: &mut App,
    ctx: &mut WgpuGfxCtx,
    debug: &DebugWindowHandles,
    frame: &Frame,
    log: &mut FrameLog,
) {
    ctx.reset(frame.width as f32, frame.height as f32);
    ctx.set_lcd_mode(agg_gui::font_settings::lcd_enabled());

    // ── Inspector wiring (View → Debug → Inspector) ─────────────────
    // Drain queued edits the inspector pushed last frame, then refresh
    // the snapshot the panel reads. Both must happen *before* layout +
    // paint so the inspector sees the post-edit tree and the snapshot
    // matches what we're about to draw.
    let t_edits = web_time::Instant::now();
    let mut edits_applied = false;
    {
        let mut q = debug.base_edits.borrow_mut();
        if !q.is_empty() {
            for edit in q.drain(..) {
                let _ = agg_gui::apply_widget_base_edit(app.root_mut(), &edit);
            }
            edits_applied = true;
            INSPECTOR_SNAPSHOT_EPOCH.with(|c| c.set(None));
        }
    }
    {
        let mut q = debug.inspector_edits.borrow_mut();
        if !q.is_empty() {
            for edit in q.drain(..) {
                let _ = agg_gui::apply_inspector_edit(app.root_mut(), &edit);
            }
            edits_applied = true;
            INSPECTOR_SNAPSHOT_EPOCH.with(|c| c.set(None));
        }
    }
    log.timings.edits_ms = elapsed_ms(t_edits);

    let t_snapshot = web_time::Instant::now();
    if debug.inspector_visible.get() {
        let epoch = agg_gui::animation::invalidation_epoch();
        let nodes_empty = debug.inspector_nodes.borrow().is_empty();
        let captured = app.has_captured_pointer();
        let should_refresh =
            nodes_empty || (!captured && INSPECTOR_SNAPSHOT_EPOCH.with(|c| c.get() != Some(epoch)));
        if should_refresh {
            *debug.inspector_nodes.borrow_mut() = app.collect_inspector_nodes();
            INSPECTOR_SNAPSHOT_EPOCH.with(|c| c.set(Some(epoch)));
        }
    } else {
        *debug.hovered_bounds.borrow_mut() = None;
        INSPECTOR_SNAPSHOT_EPOCH.with(|c| c.set(None));
    }
    log.timings.snapshot_ms = elapsed_ms(t_snapshot);

    // The shell skips layout when nothing that feeds it changed; edits
    // just applied to the live tree force one regardless.
    let t_layout = web_time::Instant::now();
    if frame.needs_layout || edits_applied {
        app.layout(frame.viewport());
    }
    log.timings.layout_ms = elapsed_ms(t_layout);

    let t_paint = web_time::Instant::now();
    app.paint(ctx);
    log.timings.paint_ms = elapsed_ms(t_paint);
}

#[inline]
fn elapsed_ms(t: web_time::Instant) -> f32 {
    t.elapsed().as_secs_f32() * 1000.0
}

/// Per-frame timing breakdown. Each span is measured around a specific
/// stage so the periodic log can attribute frame cost to the phase
/// responsible. `total_ms` is the shell's whole-frame wall clock for the
/// *previous* frame (acquire → present).
#[derive(Clone, Copy, Default)]
struct FrameTimings {
    /// Drain of `WidgetBaseEdit` + `InspectorEdit` queues from the
    /// inspector panel into the live widget tree.
    edits_ms: f32,
    /// `app.collect_inspector_nodes()` — only nonzero when the
    /// inspector is visible and the invalidation epoch changed.
    snapshot_ms: f32,
    /// `app.layout(...)` — recomputes widget bounds.
    layout_ms: f32,
    /// `app.paint(ctx)` — appends `DrawCommand`s to the deferred list.
    paint_ms: f32,
    /// Inside `end_frame`: CPU walk that turns `DrawCommand`s into
    /// `Prepared` GPU resources. From `WgpuGfxCtx::last_end_frame_stats`.
    ef_prepare_ms: f32,
    /// Inside `end_frame`: render-pass walk that records draw calls.
    ef_execute_ms: f32,
    /// Inside `end_frame`: `queue.submit()` cost.
    ef_submit_ms: f32,
    /// `DrawCommand` count from the most recent end_frame.
    cmd_count: u32,
    /// The shell's wall-clock time for the previous frame.
    total_ms: f32,
}

// ── Frame-time breakdown logger ─────────────────────────────────────
// Accumulates per-stage timings and prints an average roughly every
// 2 seconds to stderr. Off by default — set `ATOMARTIST_FRAME_LOG=1`.

const FRAME_LOG_INTERVAL: std::time::Duration = std::time::Duration::from_millis(2000);

fn frame_log_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("ATOMARTIST_FRAME_LOG")
            .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "on" | "yes"))
            .unwrap_or(false)
    })
}

/// Owns the in-flight [`FrameTimings`] and the accumulation window. One
/// per host; `paint_app_frame` fills the paint-side spans and
/// [`FrameLog::record_end_frame`] finishes the frame with the renderer's
/// end-frame stats, then feeds the periodic average.
#[derive(Default)]
pub struct FrameLog {
    timings: FrameTimings,
    acc: Vec<FrameTimings>,
    last_print: Option<web_time::Instant>,
}

impl FrameLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Called from `after_paint` — `end_frame` has run, so the renderer's
    /// per-phase split is available. Skips all work when logging is off.
    pub fn record_end_frame(&mut self, ctx: &WgpuGfxCtx, frame: &Frame) {
        if !frame_log_enabled() {
            return;
        }
        let ef = ctx.last_end_frame_stats();
        self.timings.ef_prepare_ms = ef.prepare_us as f32 / 1000.0;
        self.timings.ef_execute_ms = ef.execute_us as f32 / 1000.0;
        self.timings.ef_submit_ms = ef.submit_us as f32 / 1000.0;
        self.timings.cmd_count = ef.command_count;
        self.timings.total_ms = frame.duration.as_secs_f32() * 1000.0;
        let done = std::mem::take(&mut self.timings);
        self.acc.push(done);
        self.maybe_print();
    }

    fn maybe_print(&mut self) {
        let now = web_time::Instant::now();
        let should_log = match self.last_print {
            Some(prev) => now.duration_since(prev) >= FRAME_LOG_INTERVAL,
            None => {
                self.last_print = Some(now);
                false
            }
        };
        if !should_log || self.acc.is_empty() {
            return;
        }
        self.last_print = Some(now);
        let n = self.acc.len() as f32;
        let avg = |f: fn(&FrameTimings) -> f32| -> f32 {
            self.acc.iter().map(f).sum::<f32>() / n
        };
        let max_total = self.acc.iter().map(|t| t.total_ms).fold(0.0_f32, f32::max);
        let avg_cmds = self.acc.iter().map(|t| t.cmd_count as f32).sum::<f32>() / n;
        eprintln!(
            "[frame {:>3} samples] total(prev) avg={:.2} max={:.2} ms | edits={:.2} snapshot={:.2} layout={:.2} paint={:.2} end_frame(prep={:.2} exec={:.2} sub={:.2} cmds={:.0})",
            self.acc.len(),
            avg(|t| t.total_ms),
            max_total,
            avg(|t| t.edits_ms),
            avg(|t| t.snapshot_ms),
            avg(|t| t.layout_ms),
            avg(|t| t.paint_ms),
            avg(|t| t.ef_prepare_ms),
            avg(|t| t.ef_execute_ms),
            avg(|t| t.ef_submit_ms),
            avg_cmds,
        );
        self.acc.clear();
    }
}
