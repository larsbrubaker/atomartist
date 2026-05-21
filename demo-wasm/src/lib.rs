//! AtomArtist WASM shell — wasm-bindgen entry point + browser canvas.
//!
//! Runs the same widget tree as `demo-native` against a WebGL2 wgpu
//! surface backed by an `HtmlCanvasElement`. JS drives the animation
//! loop via `requestAnimationFrame` calling `render(w, h, frame_ms)`,
//! and forwards browser mouse events through the `on_mouse_*` exports.
//!
//! Modeled (compactly) on `agg-gui/demo-wasm/src/lib.rs` with the
//! inspector / multi-touch / persistence pieces stripped.

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::sync::Arc;

use agg_gui::{theme::{set_visuals, Visuals}, App, MouseButton, Modifiers, Size, text::Font};
use atomartist_ui::{
    build_app, fresh_state_with_starter_graph, top_menu_bar::{FileDialogProvider, NoFileDialogs},
};
use demo_wgpu::{begin_frame, WgpuGfxCtx};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

const DEFAULT_FONT_BYTES: &[u8] =
    include_bytes!("../../../agg-gui/agg-gui/assets/fonts/NotoSans-Regular.ttf");

thread_local! {
    static APP:      RefCell<Option<App>>           = RefCell::new(None);
    static WGPU_CTX: RefCell<Option<WgpuGfxCtx>>    = RefCell::new(None);
    static SURFACE:  RefCell<Option<wgpu::Surface<'static>>> = RefCell::new(None);
    static GPU:      RefCell<Option<GpuHandles>>    = RefCell::new(None);
    static SIZE:     RefCell<(u32, u32)>            = RefCell::new((0, 0));
    static CURSOR:   RefCell<(f64, f64)>            = RefCell::new((0.0, 0.0));
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

/// Browser entry point. Spawns the async wgpu init; until that resolves,
/// `render()` is a no-op (JS's animation loop just keeps polling).
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();

    // Theme + font setup mirrors the native shell.
    set_visuals(Visuals::light());
    let font = Arc::new(
        Font::from_bytes(DEFAULT_FONT_BYTES.to_vec())
            .expect("load NotoSans-Regular"),
    );
    agg_gui::font_settings::set_system_font(Some(font));
    // LCD subpixel rendering is unreliable on retina; assume hi-DPI on
    // browsers and leave it off.
    agg_gui::font_settings::set_lcd_enabled(false);
    agg_gui::font_settings::set_hinting_enabled(false);

    wasm_bindgen_futures::spawn_local(async move {
        match init_wgpu().await {
            Ok(()) => {
                log("AtomArtist WASM ready");
            }
            Err(e) => {
                web_sys::console::error_1(&JsValue::from_str(&format!(
                    "wgpu init failed: {}", e
                )));
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

    // Build wgpu instance with WebGL2 backend.
    let mut instance_desc =
        wgpu::InstanceDescriptor::new_with_display_handle(Box::new(WebDisplay));
    instance_desc.backends = wgpu::Backends::GL;
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
        .map_err(|e| format!("request_adapter: {:?}", e))?;

    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("atomartist-wasm"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_webgl2_defaults(),
            memory_hints: wgpu::MemoryHints::Performance,
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .map_err(|e| format!("request_device: {:?}", e))?;

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

    // Build the AtomArtist UI tree.
    let state = fresh_state_with_starter_graph();
    let dialogs: Arc<dyn FileDialogProvider> = Arc::new(NoFileDialogs);
    let root = build_app(state, dialogs);
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

    Ok(())
}

/// Render a single frame. JS's animation loop calls this every
/// requestAnimationFrame tick; until init resolves it's a no-op.
#[wasm_bindgen]
pub fn render(width: u32, height: u32, _frame_ms: f64) {
    let (cur_w, cur_h) = SIZE.with(|s| *s.borrow());
    let resized = cur_w != width || cur_h != height;
    if resized {
        resize_surface(width, height);
        SIZE.with(|s| *s.borrow_mut() = (width, height));
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
            let mut ctx_borrow = cc.borrow_mut();
            let mut app_borrow = ac.borrow_mut();
            if let (Some(ctx), Some(app)) = (ctx_borrow.as_mut(), app_borrow.as_mut()) {
                ctx.set_surface_texture(frame.texture.clone());
                ctx.reset(width as f32, height as f32);
                begin_frame(ctx, view);
                app.layout(Size::new(width as f64, height as f64));
                app.paint(ctx);
                ctx.end_frame();
            }
        });
    });
    frame.present();
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

#[wasm_bindgen]
pub fn on_mouse_move(x: f64, y: f64) {
    CURSOR.with(|c| *c.borrow_mut() = (x, y));
    APP.with(|c| {
        if let Some(app) = c.borrow_mut().as_mut() {
            app.on_mouse_move(x, y);
        }
    });
}

#[wasm_bindgen]
pub fn on_mouse_down(x: f64, y: f64, button: u8) {
    CURSOR.with(|c| *c.borrow_mut() = (x, y));
    let b = mouse_button_from_js(button);
    APP.with(|c| {
        if let Some(app) = c.borrow_mut().as_mut() {
            app.on_mouse_down(x, y, b, Modifiers::default());
        }
    });
}

#[wasm_bindgen]
pub fn on_mouse_up(x: f64, y: f64, button: u8) {
    CURSOR.with(|c| *c.borrow_mut() = (x, y));
    let b = mouse_button_from_js(button);
    APP.with(|c| {
        if let Some(app) = c.borrow_mut().as_mut() {
            app.on_mouse_up(x, y, b, Modifiers::default());
        }
    });
}

#[wasm_bindgen]
pub fn on_mouse_wheel(x: f64, y: f64, delta_y: f64) {
    CURSOR.with(|c| *c.borrow_mut() = (x, y));
    APP.with(|c| {
        if let Some(app) = c.borrow_mut().as_mut() {
            app.on_mouse_wheel(x, y, delta_y);
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

fn log(msg: &str) {
    web_sys::console::log_1(&JsValue::from_str(msg));
}
