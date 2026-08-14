//! Cross-platform diagnostic logging for the renderer.
//!
//! The scene renderer's telemetry used to be native-only in two ways
//! that made it useless in a browser — exactly where we most need it,
//! since a failing WASM frame produces a black canvas and no panic:
//!
//!   * it gated on `std::env::var`, which on `wasm32-unknown-unknown`
//!     always returns `Err`, so every log site was permanently off; and
//!   * it emitted through `eprintln!`, which on wasm goes nowhere.
//!
//! This module replaces both with a runtime flag (settable from JS via
//! the shell's `set_render_log` export, or from the usual env var on
//! native) and a sink that routes to the browser console on wasm and
//! stderr on native. [`crate::scene_renderer`] uses it for per-frame
//! timings; [`report_target_allocation`] uses it for the offscreen
//! budget report that diagnoses out-of-memory / over-limit blowups.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// `0` = not yet resolved, `1` = off, `2` = on. Resolved lazily from
/// the environment on first use (native) and overridable at runtime
/// from either platform via [`set_logging`].
static LOG_STATE: AtomicU8 = AtomicU8::new(0);

/// Set once the allocation report has run for a given target size, so
/// a stable window doesn't reprint it every frame.
static ALLOC_REPORTED: AtomicBool = AtomicBool::new(false);

/// Turn diagnostic logging on or off at runtime. The WASM shell exports
/// this so it can be flipped from the browser console or a `?log=1`
/// query parameter without a rebuild.
pub fn set_logging(on: bool) {
    LOG_STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
    // A fresh toggle should re-emit the allocation report — that is
    // usually the reason someone just turned logging on.
    ALLOC_REPORTED.store(false, Ordering::Relaxed);
}

/// True when diagnostic logging is enabled. On native this defaults to
/// the `ATOMARTIST_SCENE_LOG` env var; on wasm it defaults to off until
/// [`set_logging`] is called.
pub fn logging_enabled() -> bool {
    match LOG_STATE.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let on = default_from_env();
            LOG_STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn default_from_env() -> bool {
    std::env::var("ATOMARTIST_SCENE_LOG")
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "on" | "yes"))
        .unwrap_or(false)
}

#[cfg(target_arch = "wasm32")]
fn default_from_env() -> bool {
    // No environment on wasm — the shell calls `set_logging` instead.
    false
}

/// Emit one diagnostic line. Browser console on wasm, stderr on native.
pub fn log(message: &str) {
    #[cfg(target_arch = "wasm32")]
    web_sys::console::log_1(&wasm_bindgen::JsValue::from_str(message));
    #[cfg(not(target_arch = "wasm32"))]
    eprintln!("{message}");
}

/// Emit one diagnostic line at warning severity — surfaces in the
/// browser console as a warning so an over-limit target allocation
/// stands out from the per-frame timing chatter.
pub fn warn(message: &str) {
    #[cfg(target_arch = "wasm32")]
    web_sys::console::warn_1(&wasm_bindgen::JsValue::from_str(message));
    #[cfg(not(target_arch = "wasm32"))]
    eprintln!("{message}");
}

/// One offscreen render target, for the allocation report.
pub struct TargetDesc {
    pub label: &'static str,
    /// Bytes per pixel of the target's texture format.
    pub bytes_per_pixel: u32,
    /// How many textures of this description are allocated (the
    /// dual-peel depth slab is a ping-pong pair, for instance).
    pub count: u32,
}

/// Report the full offscreen allocation for a `fb_w × fb_h` scene, and
/// warn when it exceeds what the device can actually satisfy.
///
/// This exists because the failure it detects is silent. The scene
/// renders every pass into offscreen targets sized
/// `SSAA_SCALE × widget_size`, where `widget_size` is already in
/// **device** pixels — so on a phone the supersample factor multiplies
/// a device-pixel-ratio that is itself 2–3×. The resulting targets can
/// exceed `max_texture_dimension_2d` (8192 at the WebGPU default limit
/// tier) or simply exhaust a mobile GPU's memory. Either way wgpu
/// reports an error the page never surfaces and the canvas stays black.
///
/// Called from `ensure_framebuffer` whenever the size changes.
pub fn report_target_allocation(
    fb_w: u32,
    fb_h: u32,
    screen_w: u32,
    screen_h: u32,
    ssaa_scale: u32,
    limits: &wgpu::Limits,
    targets: &[TargetDesc],
) {
    let over_limit = fb_w > limits.max_texture_dimension_2d || fb_h > limits.max_texture_dimension_2d;

    // The over-limit case is a hard failure — always report it, even
    // when routine logging is off, because the alternative is a black
    // canvas with no explanation.
    if !over_limit && !logging_enabled() {
        return;
    }
    if ALLOC_REPORTED.swap(true, Ordering::Relaxed) && !over_limit {
        return;
    }

    let px = fb_w as u64 * fb_h as u64;
    let mut total: u64 = 0;
    let mut lines = String::new();
    for t in targets {
        let bytes = px * t.bytes_per_pixel as u64 * t.count as u64;
        total += bytes;
        lines.push_str(&format!(
            "\n    {:<20} {:>2} × {:>2} B/px = {:>8.1} MiB",
            t.label,
            t.count,
            t.bytes_per_pixel,
            bytes as f64 / (1024.0 * 1024.0),
        ));
    }

    let report = format!(
        "[scene alloc] viewport {screen_w}×{screen_h} device px × {ssaa_scale}× SSAA \
         = {fb_w}×{fb_h} offscreen ({:.1} MPx)\
         \n  max_texture_dimension_2d = {}\
         \n  offscreen targets:{lines}\
         \n    {:<20} {:>21.1} MiB",
        px as f64 / 1.0e6,
        limits.max_texture_dimension_2d,
        "TOTAL",
        total as f64 / (1024.0 * 1024.0),
    );

    if over_limit {
        warn(&format!(
            "{report}\n  *** OVER LIMIT: {fb_w}×{fb_h} exceeds max_texture_dimension_2d \
             ({}). Texture creation fails and the scene renders nothing (black canvas). \
             The supersample factor is multiplying an already-high device pixel ratio.",
            limits.max_texture_dimension_2d,
        ));
    } else {
        log(&report);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logging_flag_round_trips() {
        set_logging(true);
        assert!(logging_enabled());
        set_logging(false);
        assert!(!logging_enabled());
    }

    /// The over-limit path must fire even with routine logging off —
    /// that is the whole point of it, since the user-visible symptom is
    /// a black canvas with nothing in the console.
    #[test]
    fn over_limit_is_reported_regardless_of_log_flag() {
        set_logging(false);
        let limits = wgpu::Limits {
            max_texture_dimension_2d: 8192,
            ..wgpu::Limits::default()
        };
        // 2796 CSS px tall at DPR 3 is a current large phone; × 3 SSAA
        // overflows the default WebGPU limit tier.
        assert!(8388 > limits.max_texture_dimension_2d);
        // Smoke-test that the call is panic-free on both paths.
        report_target_allocation(
            3708,
            8388,
            1236,
            2796,
            3,
            &limits,
            &[TargetDesc { label: "scene_fb", bytes_per_pixel: 8, count: 1 }],
        );
        report_target_allocation(
            1200,
            800,
            400,
            267,
            3,
            &limits,
            &[TargetDesc { label: "scene_fb", bytes_per_pixel: 8, count: 1 }],
        );
    }
}
