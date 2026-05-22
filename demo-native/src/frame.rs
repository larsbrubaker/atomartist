//! Per-frame paint orchestration + frame-time logger.
//!
//! Split out of `main.rs` so the entry-point file stays under the
//! repository's 800-line guardrail
//! (`atomartist-lib/tests/file_line_count.rs`). The single public
//! entry point is [`paint_frame`]; everything else is private state
//! used to attribute frame cost across pipeline stages.

use agg_gui::{App, DrawCtx, Size};
use atomartist_ui::DebugWindowHandles;
use demo_wgpu::{begin_frame, WgpuGfxCtx};

use crate::Gpu;

// Per-frame inspector epoch tracker. Mirrors agg-gui's
// `demo-wgpu::render_app_frame` so the inspector tree only gets
// re-collected when widget invalidation actually changes — collecting
// every frame would torch the budget on a large widget tree.
thread_local! {
    static INSPECTOR_SNAPSHOT_EPOCH: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
}

/// Per-frame timing breakdown. Every span here is measured around a
/// specific stage of `paint_frame` so the periodic log can attribute
/// frame cost to the exact phase responsible. `total_ms` covers the
/// whole function body (acquire → present), so it includes the GPU
/// submit + VSync wait that `app.layout` + `app.paint` *don't* see.
#[derive(Clone, Copy, Default)]
struct FrameTimings {
    /// `surface.get_current_texture()` — blocks when the swap chain
    /// is saturated (e.g. waiting on the previous frame's present).
    acquire_ms: f32,
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
    /// `ctx.end_frame()` — prepare phase (allocates GPU buffers /
    /// bind groups for each draw command) + execute phase (records
    /// the wgpu command encoder and submits to the queue).
    end_frame_ms: f32,
    /// Inside `end_frame`: CPU walk that turns `DrawCommand`s into
    /// `Prepared` GPU resources (per-command buffer + bind-group
    /// allocation). Reported by `WgpuGfxCtx::last_end_frame_stats()`.
    ef_prepare_ms: f32,
    /// Inside `end_frame`: render-pass walk that records draw calls
    /// into the command encoder.
    ef_execute_ms: f32,
    /// Inside `end_frame`: `queue.submit()` cost.
    ef_submit_ms: f32,
    /// `DrawCommand` count from the most recent end_frame.
    cmd_count: u32,
    /// `frame.present()` — typically waits on VSync with
    /// `PresentMode::AutoVsync`.
    present_ms: f32,
    /// Wall-clock time for the whole `paint_frame` body. This is
    /// the value pushed into `SharedFrameHistory` and shown in the
    /// View → Debug → Performance Graph.
    total_ms: f32,
}

pub fn paint_frame(
    gpu: &Gpu,
    ctx: &mut WgpuGfxCtx,
    app: &mut App,
    debug: &DebugWindowHandles,
    w: u32,
    h: u32,
    capture_after: bool,
) {
    let t_total = web_time::Instant::now();
    let mut t = FrameTimings::default();

    let t_acquire = web_time::Instant::now();
    let frame = match gpu.surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
        _ => return,
    };
    t.acquire_ms = elapsed_ms(t_acquire);

    // Stash the surface texture handle before begin_frame so the screenshot
    // path can copy from it (capture_screenshot reads ctx.surface_texture).
    ctx.set_surface_texture(frame.texture.clone());
    let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
    ctx.reset(w as f32, h as f32);
    ctx.set_lcd_mode(agg_gui::font_settings::lcd_enabled());
    begin_frame(ctx, view);

    // ── Inspector wiring (View → Debug → Inspector) ─────────────────
    // Drain queued edits the inspector pushed last frame, then refresh
    // the snapshot the panel reads. Both must happen *before* layout +
    // paint so the inspector sees the post-edit tree and the snapshot
    // matches what we're about to draw.
    let t_edits = web_time::Instant::now();
    {
        let mut q = debug.base_edits.borrow_mut();
        if !q.is_empty() {
            for edit in q.drain(..) {
                let _ = agg_gui::apply_widget_base_edit(app.root_mut(), &edit);
            }
            INSPECTOR_SNAPSHOT_EPOCH.with(|c| c.set(None));
        }
    }
    {
        let mut q = debug.inspector_edits.borrow_mut();
        if !q.is_empty() {
            for edit in q.drain(..) {
                let _ = agg_gui::apply_inspector_edit(app.root_mut(), &edit);
            }
            INSPECTOR_SNAPSHOT_EPOCH.with(|c| c.set(None));
        }
    }
    t.edits_ms = elapsed_ms(t_edits);

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
    t.snapshot_ms = elapsed_ms(t_snapshot);

    let t_layout = web_time::Instant::now();
    app.layout(Size::new(w as f64, h as f64));
    t.layout_ms = elapsed_ms(t_layout);

    let t_paint = web_time::Instant::now();
    app.paint(ctx);
    t.paint_ms = elapsed_ms(t_paint);

    let t_end_frame = web_time::Instant::now();
    ctx.end_frame();
    t.end_frame_ms = elapsed_ms(t_end_frame);
    // Pull the in-renderer per-phase split so we can attribute end_frame
    // cost across prepare (per-DrawCommand buffer + bind-group allocation),
    // execute (render-pass walk), and submit (queue.submit). Different
    // dominators imply different fixes. Skip the read when logging is off
    // — the renderer still computes the numbers (cheap), we just don't
    // copy them into the per-frame struct.
    if frame_log_enabled() {
        let ef = ctx.last_end_frame_stats();
        t.ef_prepare_ms = ef.prepare_us as f32 / 1000.0;
        t.ef_execute_ms = ef.execute_us as f32 / 1000.0;
        t.ef_submit_ms = ef.submit_us as f32 / 1000.0;
        t.cmd_count = ef.command_count;
    }

    if capture_after {
        // Must run between end_frame (commands flushed) and present
        // (surface texture destroyed). The captured pixels live inside
        // ctx.capture_texture and survive present.
        ctx.capture_screenshot();
    }

    let t_present = web_time::Instant::now();
    frame.present();
    t.present_ms = elapsed_ms(t_present);

    t.total_ms = elapsed_ms(t_total);

    // The Performance graph now reflects the *full* wall-clock cost
    // per frame — including GPU submit and VSync wait — not just
    // `app.layout` + `app.paint`. That's the only number a user can
    // correlate with the perceived smoothness of the app.
    debug.frame_history.borrow_mut().push(t.total_ms);
    record_frame_timings(t);
}

#[inline]
fn elapsed_ms(t: web_time::Instant) -> f32 {
    t.elapsed().as_secs_f32() * 1000.0
}

// ── Frame-time breakdown logger ─────────────────────────────────────
// Accumulates per-stage timings and prints an average roughly every
// 2 seconds to stderr. Useful for explaining a high Performance Graph
// reading: "where did the time actually go this frame?". The averaging
// window smooths over per-frame noise; the count tells you how many
// frames went into the average so you can spot stalls (low count =
// few frames = something is slow).
//
// Off by default — set `ATOMARTIST_FRAME_LOG=1` to enable. The check is
// cheap (one atomic load per frame) and only happens after `OnceLock`
// resolves the env var on the very first frame.

const FRAME_LOG_INTERVAL: std::time::Duration = std::time::Duration::from_millis(2000);

fn frame_log_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("ATOMARTIST_FRAME_LOG")
            .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "on" | "yes"))
            .unwrap_or(false)
    })
}

thread_local! {
    static FRAME_TIMING_ACC: std::cell::RefCell<Vec<FrameTimings>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static FRAME_LOG_LAST: std::cell::Cell<Option<web_time::Instant>> =
        const { std::cell::Cell::new(None) };
}

fn record_frame_timings(t: FrameTimings) {
    if !frame_log_enabled() {
        return;
    }
    FRAME_TIMING_ACC.with(|acc| acc.borrow_mut().push(t));
    let now = web_time::Instant::now();
    let last = FRAME_LOG_LAST.with(|c| c.get());
    let should_log = match last {
        Some(prev) => now.duration_since(prev) >= FRAME_LOG_INTERVAL,
        None => {
            FRAME_LOG_LAST.with(|c| c.set(Some(now)));
            false
        }
    };
    if !should_log {
        return;
    }
    FRAME_LOG_LAST.with(|c| c.set(Some(now)));
    FRAME_TIMING_ACC.with(|acc| {
        let buf = acc.borrow();
        if buf.is_empty() {
            return;
        }
        let n = buf.len() as f32;
        let avg = |f: fn(&FrameTimings) -> f32| -> f32 {
            buf.iter().map(f).sum::<f32>() / n
        };
        let max_total = buf.iter().map(|t| t.total_ms).fold(0.0_f32, f32::max);
        let avg_cmds = buf.iter().map(|t| t.cmd_count as f32).sum::<f32>() / n;
        eprintln!(
            "[frame {:>3} samples] total avg={:.2} max={:.2} ms | acquire={:.2} edits={:.2} snapshot={:.2} layout={:.2} paint={:.2} end_frame={:.2} (prep={:.2} exec={:.2} sub={:.2} cmds={:.0}) present={:.2}",
            buf.len(),
            avg(|t| t.total_ms),
            max_total,
            avg(|t| t.acquire_ms),
            avg(|t| t.edits_ms),
            avg(|t| t.snapshot_ms),
            avg(|t| t.layout_ms),
            avg(|t| t.paint_ms),
            avg(|t| t.end_frame_ms),
            avg(|t| t.ef_prepare_ms),
            avg(|t| t.ef_execute_ms),
            avg(|t| t.ef_submit_ms),
            avg_cmds,
            avg(|t| t.present_ms),
        );
        drop(buf);
        acc.borrow_mut().clear();
    });
}
