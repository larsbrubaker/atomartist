//! Per-frame timing telemetry for the scene renderer — extracted from
//! `scene_renderer/mod.rs` to keep that file under the 800-line guardrail.
//!
//! Provides `SceneTimings`, the lightweight clock helper `elapsed_ms`, and
//! the thread-local accumulator + 1-second summary logger gated on the
//! `ATOMARTIST_SCENE_LOG` env var. Called from the WgpuCustomRender impl
//! when the env var enables it; no-ops otherwise.

pub(super) fn elapsed_ms(t: web_time::Instant) -> f32 {
    t.elapsed().as_secs_f32() * 1000.0
}

#[derive(Clone, Copy, Default)]
pub(super) struct SceneTimings {
    pub(super) total_ms: f32,
    pub(super) ensure_ms: f32,
    pub(super) fb_ms: f32,
    pub(super) mesh_ms: f32,
    pub(super) bed_composite_ms: f32,
    pub(super) bed_ran_chain: bool,
    pub(super) peel_ms: f32,
    pub(super) blit_ms: f32,
}

fn scene_log_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("ATOMARTIST_SCENE_LOG")
            .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "on" | "yes"))
            .unwrap_or(false)
    })
}

thread_local! {
    static SCENE_TIMING_ACC: std::cell::RefCell<Vec<SceneTimings>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static SCENE_LOG_LAST: std::cell::Cell<Option<web_time::Instant>> =
        const { std::cell::Cell::new(None) };
}

const SCENE_LOG_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1000);

pub(super) fn log_scene_timings(t: SceneTimings) {
    if !scene_log_enabled() {
        return;
    }
    SCENE_TIMING_ACC.with(|acc| acc.borrow_mut().push(t));
    let now = web_time::Instant::now();
    let last = SCENE_LOG_LAST.with(|c| c.get());
    let should_log = match last {
        Some(prev) => now.duration_since(prev) >= SCENE_LOG_INTERVAL,
        None => {
            SCENE_LOG_LAST.with(|c| c.set(Some(now)));
            false
        }
    };
    if !should_log {
        return;
    }
    SCENE_LOG_LAST.with(|c| c.set(Some(now)));
    SCENE_TIMING_ACC.with(|acc| {
        let buf = acc.borrow();
        if buf.is_empty() {
            return;
        }
        let n = buf.len() as f32;
        let avg = |f: fn(&SceneTimings) -> f32| -> f32 { buf.iter().map(f).sum::<f32>() / n };
        let max_total = buf.iter().map(|t| t.total_ms).fold(0.0_f32, f32::max);
        let chain_hits = buf.iter().filter(|t| t.bed_ran_chain).count();
        eprintln!(
            "[scene {:>3} frames] total avg={:.2} max={:.2} ms | ensure={:.3} fb={:.3} mesh={:.3} bed_comp={:.3} peel={:.3} downsample={:.3} | chain_runs={}/{}",
            buf.len(),
            avg(|t| t.total_ms),
            max_total,
            avg(|t| t.ensure_ms),
            avg(|t| t.fb_ms),
            avg(|t| t.mesh_ms),
            avg(|t| t.bed_composite_ms),
            avg(|t| t.peel_ms),
            avg(|t| t.blit_ms),
            chain_hits,
            buf.len(),
        );
        drop(buf);
        acc.borrow_mut().clear();
    });
}
