//! AtomArtist WASM shell — wasm-bindgen entry point + browser canvas.
//!
//! Runs the same widget tree as `demo-native` against a WebGL2 wgpu
//! surface backed by an `HtmlCanvasElement`. JS drives the animation
//! loop via `requestAnimationFrame` calling `render(w, h, frame_ms)`,
//! and forwards browser mouse events through the `on_mouse_*` exports.
//! Keyboard input needs no JS glue: [`install_keyboard`] hands the DOM
//! `keydown` / `keyup` / clipboard plumbing to agg-gui's `web_adapter`,
//! which feeds `App::on_key_down` / `on_key_up` directly.
//!
//! UI settings survive page reloads via [`web_settings`] (localStorage),
//! which also drives the startup auto-reopen of the last project — the
//! web mirror of `demo-native`'s `shell_settings` + `AutoSave` pair.
//!
//! Modeled (compactly) on `agg-gui/demo-wasm/src/lib.rs` with the
//! inspector / multi-touch pieces stripped.

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::sync::Arc;

use agg_gui::{App, MouseButton, Modifiers, Size};
use atomartist_storage::{BrowserProvider, StorageRegistry};
use atomartist_ui::{
    build_app, fresh_state_with_starter_graph_and_storage, install_theme_and_fonts,
    top_menu_bar::FileDialogProvider, DebugWindowHandles, FirstPaintGate,
};

use demo_wgpu::{begin_frame, WgpuGfxCtx};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

mod dialogs;
pub mod web_lifecycle;
pub mod web_settings;
use dialogs::WebDialogs;

thread_local! {
    static APP:      RefCell<Option<App>>           = RefCell::new(None);
    static WGPU_CTX: RefCell<Option<WgpuGfxCtx>>    = RefCell::new(None);
    static SURFACE:  RefCell<Option<wgpu::Surface<'static>>> = RefCell::new(None);
    static GPU:      RefCell<Option<GpuHandles>>    = RefCell::new(None);
    static SIZE:     RefCell<(u32, u32)>            = RefCell::new((0, 0));
    static CURSOR:   RefCell<(f64, f64)>            = RefCell::new((0.0, 0.0));
    // View → Debug window handles (inspector + performance). Set on
    // wgpu init; consumed each frame by `render` for edit draining,
    // node snapshotting, and `FrameHistory::push`.
    static DEBUG:    RefCell<Option<DebugWindowHandles>> = RefCell::new(None);
    // Clone of the AppState the widget tree was built over — the shell
    // keeps one so `render` can drive the storage job pump (the widget
    // tree has no way to reach it from outside an event).
    static STATE:    RefCell<Option<atomartist_ui::AppState>> = RefCell::new(None);
    // Mirrors agg-gui's `render_app_frame::INSPECTOR_SNAPSHOT_EPOCH`
    // so we only re-collect when widget invalidation changes.
    static INSPECTOR_SNAPSHOT_EPOCH: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
    // Forces `render` to paint until one frame has actually been
    // presented — see the `FirstPaintGate` docs and the note at the end
    // of `init_wgpu`. Belt and braces alongside the `request_draw()`
    // there: this one survives anything else consuming the draw flag
    // between init and the next requestAnimationFrame tick.
    static FIRST_PAINT: FirstPaintGate = const { FirstPaintGate::new() };
}

struct GpuHandles {
    device: Arc<wgpu::Device>,
    // Held only to keep the queue alive for the lifetime of the
    // surface; resize_surface() only needs `device` + `surface_format`.
    _queue: Arc<wgpu::Queue>,
    surface_format: wgpu::TextureFormat,
}

/// Zero-sized `HasDisplayHandle` shim so wgpu 29 accepts our canvas
/// surface (canvas legitimately has no display, but wgpu-core requires
/// one of the two display sources to be Some). Same workaround agg-gui's
/// demo-wasm uses.
#[derive(Debug)]
struct WebDisplay;
impl wgpu::rwh::HasDisplayHandle for WebDisplay {
    fn display_handle(
        &self,
    ) -> Result<wgpu::rwh::DisplayHandle<'_>, wgpu::rwh::HandleError> {
        Ok(wgpu::rwh::DisplayHandle::web())
    }
}

/// Replace the canvas with a readable error panel — users without
/// WebGPU should see *why* the demo is blank, not a dead canvas with
/// a console-only error.
fn show_fatal(message: &str) {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let Some(canvas) = document.get_element_by_id("canvas") else {
        return;
    };
    if let Ok(panel) = document.create_element("div") {
        panel.set_attribute(
            "style",
            "max-width:40em;margin:4em auto;padding:1.5em 2em;\
             font:16px/1.5 system-ui,sans-serif;color:#333;\
             background:#fff3f0;border:1px solid #e0b4a8;border-radius:8px;",
        )
        .ok();
        panel.set_text_content(Some(message));
        canvas.replace_with_with_node_1(&panel).ok();
    }
}

/// Browser entry point. Spawns the async wgpu init; until that resolves,
/// `render()` is a no-op (JS's animation loop just keeps polling).
/// Toggle the renderer's diagnostic logging at runtime. Callable from
/// the browser console (`wasm.set_render_log(true)`) or automatically
/// via a `?log=1` query parameter — see [`start`]. Enables the
/// per-second scene-timing summary and the offscreen allocation report,
/// both of which are otherwise silent.
///
/// The native shell uses `ATOMARTIST_SCENE_LOG=1` for the same thing;
/// there is no environment to read on wasm, hence this export.
#[wasm_bindgen]
pub fn set_render_log(on: bool) {
    atomartist_renderer::diagnostics::set_logging(on);
}

/// True when the page URL carries `?log=1` (or `&log=1`), so a phone
/// with no console access can still be told to emit diagnostics.
fn log_requested_by_url() -> bool {
    web_sys::window()
        .and_then(|w| w.location().search().ok())
        .map(|q| q.contains("log=1"))
        .unwrap_or(false)
}

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();

    if log_requested_by_url() {
        atomartist_renderer::diagnostics::set_logging(true);
    }

    // Register the browser's device-pixel ratio as the agg-gui device scale
    // *before* installing fonts, so layout, hit-testing, and the LCD/hinting
    // DPI decision all use the same value the native shell derives from
    // `window.scale_factor()`. The JS bootstrap sizes the canvas backing
    // store at `clientSize * devicePixelRatio` to match.
    let device_scale = web_sys::window()
        .map(|w| w.device_pixel_ratio())
        .filter(|s| *s > 0.0)
        .unwrap_or(1.0);
    agg_gui::set_device_scale(device_scale);

    // Theme, fonts, and the full text-quality recipe — shared verbatim with
    // the native shell so the two render pixel-identically.
    install_theme_and_fonts(device_scale);

    wasm_bindgen_futures::spawn_local(async move {
        match init_wgpu().await {
            Ok(()) => {
                log("AtomArtist WASM ready");
            }
            Err(e) => {
                web_sys::console::error_1(&JsValue::from_str(&format!(
                    "wgpu init failed: {}", e
                )));
                show_fatal(&e);
            }
        }
    });
}

async fn init_wgpu() -> Result<(), String> {
    let document = web_sys::window()
        .ok_or("no global window")?
        .document()
        .ok_or("no document")?;
    let canvas = document
        .get_element_by_id("canvas")
        .ok_or("canvas element not found (need <canvas id=\"canvas\">)")?
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .map_err(|_| "element is not a canvas")?;
    let initial_size = (canvas.width(), canvas.height());
    SIZE.with(|s| *s.borrow_mut() = initial_size);

    // Browser WebGPU backend only. The scene renderer's opaque pass
    // writes two colour attachments with different blend/write-mask
    // states (INDEPENDENT_BLEND) — WebGL2 cannot express that and
    // panics creating the scene pipeline, so there is no GL fallback;
    // browsers without WebGPU get a clear message instead.
    let mut instance_desc =
        wgpu::InstanceDescriptor::new_with_display_handle(Box::new(WebDisplay));
    instance_desc.backends = wgpu::Backends::BROWSER_WEBGPU;
    let instance = wgpu::Instance::new(instance_desc);

    let surface = instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
        .map_err(|e| format!("create_surface: {:?}", e))?;

    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        })
        .await
        .map_err(|_| {
            "WebGPU is not available in this browser. AtomArtist needs WebGPU \
             (Chrome/Edge 113+, Firefox 141+, Safari 26+ — or enable it in \
             your browser's settings)."
                .to_string()
        })?;

    // Enable 32-bit-float blending when the browser exposes it
    // (`EXT_float_blend`, widely available on WebGL2/WebGPU). The
    // dual-peel chain needs it to separate perspective-compressed
    // transparent layers by depth; without it we fall back to
    // half-float depth. Requested only when present so init never fails.
    let float32_blend = adapter.features() & wgpu::Features::FLOAT32_BLENDABLE;
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("atomartist-wasm"),
            required_features: float32_blend,
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .map_err(|e| format!("request_device: {:?}", e))?;

    // Uncaptured errors are the difference between "black canvas, no
    // explanation" and a diagnosable bug. wgpu reports texture
    // allocation failures, over-limit sizes, and shader-module
    // rejections through this channel; without a handler they vanish
    // and the frame silently produces nothing. This is the single most
    // useful thing to have installed when triaging a blank page on a
    // device we can't attach a debugger to.
    device.on_uncaptured_error(std::sync::Arc::new(|e: wgpu::Error| {
        let text = format!("wgpu uncaptured error: {e}");
        web_sys::console::error_1(&JsValue::from_str(&text));
    }));

    // One-shot capability dump — adapter, backend, and the limits the
    // renderer's offscreen budget is measured against.
    let info = adapter.get_info();
    let limits = device.limits();
    log(&format!(
        "GPU: {} ({:?}, {:?}) | max_texture_dimension_2d={} \
         max_buffer_size={} MiB | float32-blend={} | dpr={}",
        info.name,
        info.backend,
        info.device_type,
        limits.max_texture_dimension_2d,
        limits.max_buffer_size / (1024 * 1024),
        !float32_blend.is_empty(),
        web_sys::window().map(|w| w.device_pixel_ratio()).unwrap_or(1.0),
    ));

    let caps = surface.get_capabilities(&adapter);
    let surface_format = caps
        .formats
        .iter()
        .copied()
        .find(|f| !f.is_srgb())
        .unwrap_or(caps.formats[0]);

    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: surface_format,
        width: initial_size.0.max(1),
        height: initial_size.1.max(1),
        present_mode: wgpu::PresentMode::AutoVsync,
        desired_maximum_frame_latency: 2,
        alpha_mode: caps.alpha_modes[0],
        view_formats: vec![],
    };
    surface.configure(&device, &config);

    let device_arc = Arc::new(device);
    let queue_arc = Arc::new(queue);
    let wgpu_ctx = WgpuGfxCtx::new(
        device_arc.clone(),
        queue_arc.clone(),
        surface_format,
        initial_size.0 as f32,
        initial_size.1 as f32,
    );

    // Load persisted UI settings before anything reads them. Missing,
    // unparseable, or unreachable storage silently falls back to the
    // documented defaults — never blocks startup (see `web_settings`).
    // The raw blob is kept to seed AutoSave so the first painted frame
    // doesn't rewrite an identical value.
    let stored_blob = web_settings::read_settings_blob();
    let loaded_settings = web_settings::settings_from_stored(stored_blob.as_deref());
    if let Some(blob) = stored_blob {
        web_lifecycle::seed_auto_save(blob);
    }

    // Storage backends this shell offers, the web mirror of
    // `demo-native`'s registry: the browser gets the origin's private
    // file system under the `browser:` scheme. `atomartist-ui` itself
    // registers nothing, so the choice lives here in the shell.
    let storage = {
        let mut registry = StorageRegistry::new();
        registry
            .register(Arc::new(BrowserProvider::new()))
            .expect("fresh registry accepts the browser storage provider");
        Arc::new(registry)
    };
    let state = fresh_state_with_starter_graph_and_storage(storage);
    // Apply the restored HUD state (perspective / turntable / bed /
    // render style / snap / theme / accent / recents) *before* mounting
    // the widget tree, so the first paint reflects what the user left
    // things at — same ordering as `demo-native::main`.
    state.apply_ui_settings(&loaded_settings);
    // Auto-reopen the last project. Ordering matters: the `browser:`
    // provider is registered above, so the URI resolves. Unlike native,
    // this open is genuinely asynchronous — OPFS settles on the browser
    // event loop — so it lands a few frames in via `render`'s
    // `pump_storage`, and the starter graph is what's on screen until
    // then. Failure is non-fatal and reported at Info level by
    // `reopen_last_project` (a project cleared out of OPFS since the
    // last visit is news, not a fault).
    if let Some(last) = loaded_settings.last_project_path.as_ref() {
        state.reopen_last_project(last);
    }
    // Placeholder pickers (see `dialogs.rs`): save goes to a fixed
    // `browser:///projects/…` location so Ctrl+S persists, and open /
    // import still do nothing until the in-app file browser lands in
    // Phase 6.
    let dialogs: Arc<dyn FileDialogProvider> = Arc::new(WebDialogs);
    // Handle to the in-app Open/Save picker — step 6c-2 hands it to the
    // dialog provider that replaces `WebDialogs`.
    let browser_modal = atomartist_ui::file_browser::FileBrowserModalHandle::new();
    let (root, debug) = build_app(
        state.clone(),
        dialogs,
        Some(loaded_settings),
        browser_modal,
    );
    STATE.with(|c| *c.borrow_mut() = Some(state));
    let app = App::new(root);

    GPU.with(|c| {
        *c.borrow_mut() = Some(GpuHandles {
            device: device_arc,
            _queue: queue_arc,
            surface_format,
        });
    });
    SURFACE.with(|c| *c.borrow_mut() = Some(surface));
    WGPU_CTX.with(|c| *c.borrow_mut() = Some(wgpu_ctx));
    APP.with(|c| *c.borrow_mut() = Some(app));
    DEBUG.with(|c| *c.borrow_mut() = Some(debug));

    install_keyboard();

    // Lifecycle listeners, both registered from Rust so `index.html`
    // stays a canvas-events-only shim: a window-level mouse-release
    // watcher that keeps the persistence idle guard from wedging when a
    // drag ends outside the canvas, and the hidden/pagehide flush that
    // gives the web shell native's write-on-close behaviour.
    web_lifecycle::install_window_mouse_release_listener();
    web_lifecycle::install_page_hide_flush(|| persist_settings_if_changed(true));

    // The web equivalent of winit's initial `RedrawRequested`. The app
    // defaults to Reactive mode, whose paint gate (`should_paint`) only
    // fires on an animation, an invalidation, or a due deadline —
    // nothing guarantees one of those is pending at the instant this
    // async init resolves. Without this the page could stay blank until
    // the first resize (whose code path bypasses the gate), which is
    // exactly what a refresh — and every mobile load — hit.
    agg_gui::animation::request_draw();

    Ok(())
}

/// Render a single frame. JS's animation loop calls this every
/// requestAnimationFrame tick; until init resolves it's a no-op.
///
/// `frame_ms` is the interval JS measured between this callback and the
/// last one. It is *not* what the Performance graph plots — see
/// [`should_paint`] and the timing push below.
#[wasm_bindgen]
pub fn render(width: u32, height: u32, frame_ms: f64) {
    let _ = frame_ms;
    let t_frame = web_time::Instant::now();

    let (cur_w, cur_h) = SIZE.with(|s| *s.borrow());
    let resized = cur_w != width || cur_h != height;
    if resized {
        resize_surface(width, height);
        SIZE.with(|s| *s.borrow_mut() = (width, height));
    }

    // Honour the Performance window's Reactive / Continuous selector.
    // requestAnimationFrame fires at vsync no matter what the app
    // needs, so without this gate the WASM shell painted every single
    // tick and Reactive mode did nothing — the mode switch was inert on
    // web while working correctly on native.
    //
    // The first-paint gate sits in front of it: until one frame has
    // been presented we paint regardless, because Reactive mode has no
    // guaranteed first-frame signal on the web. The gate takes
    // `should_paint` lazily and skips it while forcing a paint, so a due
    // `request_draw_after` deadline isn't promoted on a tick that may
    // still bail before painting. That's tidiness, not correctness:
    // `wants_draw()` never clears the immediate flag.
    // Storage job pump, ahead of the paint gate: a provider that
    // completed since the last tick must have its continuation applied
    // even on a frame that is about to bail out without painting. The
    // pump re-requests a draw while anything is still in flight, so the
    // gate below lets the next tick through.
    //
    // The state is cloned out of the RefCell and the borrow dropped
    // before pumping: continuations are arbitrary app code and may well
    // reach for STATE themselves, which would panic on the outstanding
    // borrow. `AppState` is a bundle of `Arc`s, so the clone is cheap and
    // shares everything that matters.
    let pump_state = STATE.with(|c| c.borrow().clone());
    if let Some(state) = pump_state {
        state.pump_storage();
    }

    if !FIRST_PAINT.with(|g| g.should_paint_tick(resized, should_paint)) {
        return;
    }

    let acquired = SURFACE.with(|c| {
        c.borrow().as_ref().map(|s| s.get_current_texture())
    });
    let frame = match acquired {
        Some(wgpu::CurrentSurfaceTexture::Success(f))
        | Some(wgpu::CurrentSurfaceTexture::Suboptimal(f)) => f,
        _ => return,
    };
    let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
    WGPU_CTX.with(|cc| {
        APP.with(|ac| {
            DEBUG.with(|dc| {
                let mut ctx_borrow = cc.borrow_mut();
                let mut app_borrow = ac.borrow_mut();
                let debug_borrow = dc.borrow();
                if let (Some(ctx), Some(app), Some(debug)) = (
                    ctx_borrow.as_mut(),
                    app_borrow.as_mut(),
                    debug_borrow.as_ref(),
                ) {
                    ctx.set_surface_texture(frame.texture.clone());
                    ctx.reset(width as f32, height as f32);
                    begin_frame(ctx, view);

                    // Inspector edit drain + snapshot refresh (same
                    // dance as `demo-native::paint_frame`).
                    {
                        let mut q = debug.base_edits.borrow_mut();
                        if !q.is_empty() {
                            for edit in q.drain(..) {
                                let _ =
                                    agg_gui::apply_widget_base_edit(app.root_mut(), &edit);
                            }
                            INSPECTOR_SNAPSHOT_EPOCH.with(|c| c.set(None));
                        }
                    }
                    {
                        let mut q = debug.inspector_edits.borrow_mut();
                        if !q.is_empty() {
                            for edit in q.drain(..) {
                                let _ =
                                    agg_gui::apply_inspector_edit(app.root_mut(), &edit);
                            }
                            INSPECTOR_SNAPSHOT_EPOCH.with(|c| c.set(None));
                        }
                    }
                    if debug.inspector_visible.get() {
                        let epoch = agg_gui::animation::invalidation_epoch();
                        let nodes_empty = debug.inspector_nodes.borrow().is_empty();
                        let captured = app.has_captured_pointer();
                        let should_refresh = nodes_empty
                            || (!captured
                                && INSPECTOR_SNAPSHOT_EPOCH
                                    .with(|c| c.get() != Some(epoch)));
                        if should_refresh {
                            *debug.inspector_nodes.borrow_mut() =
                                app.collect_inspector_nodes();
                            INSPECTOR_SNAPSHOT_EPOCH.with(|c| c.set(Some(epoch)));
                        }
                    } else {
                        *debug.hovered_bounds.borrow_mut() = None;
                        INSPECTOR_SNAPSHOT_EPOCH.with(|c| c.set(None));
                    }

                    app.layout(Size::new(width as f64, height as f64));
                    app.paint(ctx);
                    ctx.end_frame();

                    // Plot the frame cost we measured ourselves, which
                    // is what `demo-native::paint_frame` pushes too, so
                    // the two platforms' Performance graphs mean the
                    // same thing and can be compared directly.
                    //
                    // This used to push JS's `frame_ms` argument, which
                    // the page was supplying as the raw
                    // requestAnimationFrame *timestamp* rather than an
                    // interval — so the readout showed milliseconds
                    // since page load (six-figure numbers that climbed
                    // forever) instead of frame time.
                    let elapsed = t_frame.elapsed().as_secs_f32() * 1000.0;
                    if elapsed.is_finite() {
                        debug.frame_history.borrow_mut().push(elapsed);
                    }
                }
            });
        });
    });
    frame.present();
    // Latched only here, after a frame was acquired, painted, and
    // presented — every early `return` above leaves the gate open so
    // the next tick tries again.
    FIRST_PAINT.with(|g| g.mark_painted());
    persist_settings_if_changed(false);
}

/// Persist the settings blob to `localStorage` when it differs from what
/// is already stored.
///
/// Called once per *painted* frame with `force = false`, the web
/// equivalent of the native shell's post-paint `AutoSave` tick: every
/// settings change originates in an interaction that invalidates the
/// widget tree, so a painted frame always follows. Idle frames skip the
/// compose entirely, and the diff inside `AutoSave` means a steady state
/// writes nothing. In that mode the write also waits for no mouse button
/// to be held, so a blob is never captured halfway through a drag.
///
/// `force = true` is the page-hide path (see
/// [`web_lifecycle::install_page_hide_flush`]), the web counterpart of
/// native's write-on-close: `visibilitychange` → hidden and `pagehide`
/// are the documented teardown hooks, and both must write even mid-drag
/// because there may be no further frame.
fn persist_settings_if_changed(force: bool) {
    let Some(state) = STATE.with(|c| c.borrow().clone()) else {
        return;
    };
    let Some(debug) = DEBUG.with(|c| c.borrow().clone()) else {
        return;
    };
    web_lifecycle::tick_auto_save(
        force,
        || web_settings::compose_settings_blob(&state, &debug),
        web_settings::write_settings_blob,
    );
}

/// Whether this requestAnimationFrame tick should actually paint.
///
/// Ports `demo-native`'s redraw policy (see its `AboutToWait` /
/// post-paint arms) to the web loop, so the Reactive / Continuous
/// selector behaves identically on both platforms:
///
///   * Continuous — always paint, so the FPS readout reflects a real
///     sustained framerate;
///   * Reactive — paint only when an animation, a widget invalidation,
///     or a scheduled draw deadline asks for it.
///
/// Returning `false` skips the surface acquire and the whole
/// layout/paint, which is the point: in Reactive mode an idle page
/// should cost nothing per vsync tick. The rAF loop keeps running
/// either way, so the next tick picks up any new invalidation — that is
/// the web equivalent of winit's `request_redraw`.
fn should_paint() -> bool {
    let continuous = DEBUG.with(|dc| {
        dc.borrow()
            .as_ref()
            .map(|d| d.run_mode.get() == agg_gui::RunMode::Continuous)
            // Before init completes there is nothing to paint anyway.
            .unwrap_or(false)
    });
    if continuous || agg_gui::animation::wants_draw() {
        return true;
    }
    APP.with(|ac| {
        let mut app_borrow = ac.borrow_mut();
        let Some(app) = app_borrow.as_mut() else {
            return false;
        };
        if app.wants_draw() {
            return true;
        }
        // A widget may have asked to be redrawn at a future instant
        // (caret blink, delayed tooltip). Paint once that deadline
        // passes, matching native's `next_scheduled_redraw`.
        let deadline = match (
            agg_gui::animation::peek_next_draw_deadline(),
            app.next_draw_deadline(),
        ) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        deadline.is_some_and(|d| web_time::Instant::now() >= d)
    })
}

fn resize_surface(width: u32, height: u32) {
    GPU.with(|gc| {
        SURFACE.with(|sc| {
            let gpu_borrow = gc.borrow();
            let surface_borrow = sc.borrow();
            if let (Some(gpu), Some(surface)) = (gpu_borrow.as_ref(), surface_borrow.as_ref()) {
                let config = wgpu::SurfaceConfiguration {
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    format: gpu.surface_format,
                    width: width.max(1),
                    height: height.max(1),
                    present_mode: wgpu::PresentMode::AutoVsync,
                    desired_maximum_frame_latency: 2,
                    alpha_mode: wgpu::CompositeAlphaMode::Auto,
                    view_formats: vec![],
                };
                surface.configure(&gpu.device, &config);
            }
        });
    });
    WGPU_CTX.with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            ctx.reset(width as f32, height as f32);
        }
    });
}

/// Wire physical-keyboard input (and the clipboard bridge that rides
/// along with it) into the live `App`.
///
/// The native shell gets keys from winit; on the web the equivalent
/// plumbing lives in agg-gui's `web_adapter`, which registers
/// window-level `keydown` / `keyup` / `copy` / `cut` / `paste`
/// listeners, translates `KeyboardEvent.key` into [`agg_gui::Key`],
/// and `preventDefault()`s only the keys an app actually consumes
/// (typing / navigation / the Ctrl-C,X,A,Z,Y set) so Tab, F5, F12 and
/// other browser chrome keep working. Using it keeps the shell pure
/// Rust — no keyboard glue in `index.html`.
///
/// Listeners are window-level, so no canvas `tabindex` / focus dance is
/// needed: keys reach the app whether or not the canvas is focused,
/// except while a real DOM editor (`<input>`, `contenteditable`, …) has
/// focus, which the adapter deliberately leaves to the browser.
///
/// Like the mouse handlers, this doesn't force a repaint: widgets that
/// change visually mark themselves dirty from `on_event`, and the
/// requestAnimationFrame loop's paint gate picks that up.
fn install_keyboard() {
    agg_gui::web_adapter::install_keyboard_listeners(|key, mods, pressed| {
        APP.with(|c| {
            if let Some(app) = c.borrow_mut().as_mut() {
                if pressed {
                    app.on_key_down(key, mods);
                } else {
                    app.on_key_up(key, mods);
                }
            }
        });
    });
}

/// Forward a canvas `mousemove`.
///
/// `buttons` is the DOM event's `MouseEvent.buttons` bitmask, forwarded
/// so the shell can re-derive how many buttons are actually held rather
/// than trusting its own down/up counting — see
/// [`web_lifecycle::sync_mouse_buttons`]. A caller that has no bitmask
/// to offer can pass `0`; that reads as "nothing held", which only ever
/// lets settings persist a little eagerly.
#[wasm_bindgen]
pub fn on_mouse_move(x: f64, y: f64, buttons: u32) {
    CURSOR.with(|c| *c.borrow_mut() = (x, y));
    web_lifecycle::sync_mouse_buttons(buttons);
    APP.with(|c| {
        if let Some(app) = c.borrow_mut().as_mut() {
            app.on_mouse_move(x, y);
        }
    });
}

#[wasm_bindgen]
pub fn on_mouse_down(x: f64, y: f64, button: u8, ctrl: bool, shift: bool, alt: bool, meta: bool) {
    CURSOR.with(|c| *c.borrow_mut() = (x, y));
    let b = mouse_button_from_js(button);
    let mods = modifiers_from_js(ctrl, shift, alt, meta);
    // Opens the settings idle guard. It is *not* this counter that keeps
    // the guard honest — a release delivered outside the canvas never
    // reaches `on_mouse_up` — the `buttons` resync on the next
    // `mousemove` and the window-level release listener are.
    web_lifecycle::note_mouse_down();
    APP.with(|c| {
        if let Some(app) = c.borrow_mut().as_mut() {
            app.on_mouse_down(x, y, b, mods);
        }
    });
}

#[wasm_bindgen]
pub fn on_mouse_up(x: f64, y: f64, button: u8, ctrl: bool, shift: bool, alt: bool, meta: bool) {
    CURSOR.with(|c| *c.borrow_mut() = (x, y));
    let b = mouse_button_from_js(button);
    let mods = modifiers_from_js(ctrl, shift, alt, meta);
    web_lifecycle::note_mouse_up();
    APP.with(|c| {
        if let Some(app) = c.borrow_mut().as_mut() {
            app.on_mouse_up(x, y, b, mods);
        }
    });
}

#[wasm_bindgen]
pub fn on_mouse_wheel(x: f64, y: f64, delta_y: f64, ctrl: bool, shift: bool, alt: bool, meta: bool) {
    CURSOR.with(|c| *c.borrow_mut() = (x, y));
    let mods = modifiers_from_js(ctrl, shift, alt, meta);
    APP.with(|c| {
        if let Some(app) = c.borrow_mut().as_mut() {
            // `_xy_mods` rather than the 2-arg `on_mouse_wheel`: without
            // the modifiers, Ctrl+wheel zoom and Shift+wheel horizontal
            // scroll were dead on web. delta_x is 0 — the JS glue only
            // forwards vertical wheel today.
            app.on_mouse_wheel_xy_mods(x, y, 0.0, delta_y, mods);
        }
    });
}

fn mouse_button_from_js(b: u8) -> MouseButton {
    match b {
        0 => MouseButton::Left,
        1 => MouseButton::Middle,
        2 => MouseButton::Right,
        n => MouseButton::Other(n),
    }
}

/// Build agg-gui [`Modifiers`] from the raw `MouseEvent` booleans the JS
/// glue forwards.
///
/// These come from the mouse event itself rather than from cached
/// keyboard state: the DOM event's `ctrlKey` / `shiftKey` / … are
/// authoritative even when the page lost and regained focus mid-drag (an
/// Alt+Tab away and back never delivers the matching `keyup`, so cached
/// key state would report a modifier that is no longer held). Same
/// reasoning as the native shell reading winit's `ModifiersChanged`
/// rather than tracking key events itself.
fn modifiers_from_js(ctrl: bool, shift: bool, alt: bool, meta: bool) -> Modifiers {
    Modifiers {
        ctrl,
        shift,
        alt,
        meta,
    }
}

fn log(msg: &str) {
    web_sys::console::log_1(&JsValue::from_str(msg));
}
