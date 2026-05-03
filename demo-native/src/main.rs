//! AtomArtist native shell — winit + wgpu.
//!
//! Mounts the shared widget tree from `atomartist-ui` onto a winit window
//! using the wgpu DrawCtx from `demo-wgpu`. No application logic lives
//! here — see `atomartist-ui::build_app` for the widget tree.
//!
//! Modeled (compactly) on `agg-gui/demo-native/src/main.rs` minus the
//! inspector / screenshot / MSAA / multi-touch / font-asset machinery
//! which AtomArtist doesn't need yet.

use std::sync::Arc;

use agg_gui::{App, Key, Modifiers, MouseButton, Size, text::Font};
use atomartist_ui::{build_app, fresh_state_with_starter_graph};
use demo_wgpu::{begin_frame, WgpuGfxCtx};
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Event, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::window::{Window, WindowAttributes};

const DEFAULT_FONT_BYTES: &[u8] =
    include_bytes!("../../../agg-gui/agg-gui/assets/fonts/NotoSans-Regular.ttf");

struct Gpu {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    surface: wgpu::Surface<'static>,
    surface_format: wgpu::TextureFormat,
    config: wgpu::SurfaceConfiguration,
}

impl Gpu {
    fn new(window: Arc<Window>) -> Self {
        let size = window.inner_size();
        let mut instance_desc = wgpu::InstanceDescriptor::new_without_display_handle();
        instance_desc.backends = wgpu::Backends::PRIMARY;
        let instance = wgpu::Instance::new(instance_desc);
        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("request adapter");

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("atomartist-native-wgpu"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        }))
        .expect("request device");

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
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        Self {
            device: Arc::new(device),
            queue: Arc::new(queue),
            surface,
            surface_format,
            config,
        }
    }

    fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 { return; }
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.device, &self.config);
    }
}

fn paint_frame(gpu: &Gpu, ctx: &mut WgpuGfxCtx, app: &mut App, w: u32, h: u32) {
    let frame = match gpu.surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
        _ => return,
    };
    let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
    ctx.reset(w as f32, h as f32);
    begin_frame(ctx, view);
    app.layout(Size::new(w as f64, h as f64));
    app.paint(ctx);
    ctx.end_frame();
    frame.present();
}

fn translate_winit_button(b: winit::event::MouseButton) -> Option<MouseButton> {
    use winit::event::MouseButton as W;
    match b {
        W::Left => Some(MouseButton::Left),
        W::Middle => Some(MouseButton::Middle),
        W::Right => Some(MouseButton::Right),
        W::Other(n) => Some(MouseButton::Other(n as u8)),
        _ => None,
    }
}

fn translate_winit_key(key: &winit::keyboard::Key) -> Option<Key> {
    use winit::keyboard::{Key as W, NamedKey};
    match key {
        W::Character(s) => s.chars().next().map(Key::Char),
        W::Named(n) => match n {
            NamedKey::Backspace => Some(Key::Backspace),
            NamedKey::Delete => Some(Key::Delete),
            NamedKey::Insert => Some(Key::Insert),
            NamedKey::ArrowLeft => Some(Key::ArrowLeft),
            NamedKey::ArrowRight => Some(Key::ArrowRight),
            NamedKey::ArrowUp => Some(Key::ArrowUp),
            NamedKey::ArrowDown => Some(Key::ArrowDown),
            NamedKey::Home => Some(Key::Home),
            NamedKey::End => Some(Key::End),
            NamedKey::Tab => Some(Key::Tab),
            NamedKey::Enter => Some(Key::Enter),
            NamedKey::Escape => Some(Key::Escape),
            NamedKey::Space => Some(Key::Char(' ')),
            _ => None,
        },
        _ => None,
    }
}

#[allow(deprecated)]
fn main() {
    let event_loop = EventLoop::new().expect("event loop");

    let font = Arc::new(
        Font::from_bytes(DEFAULT_FONT_BYTES.to_vec()).expect("load NotoSans-Regular"),
    );
    let _ = font; // installed via agg-gui's font subsystem on first widget that needs it

    let window_attributes = WindowAttributes::default()
        .with_title("AtomArtist")
        .with_inner_size(LogicalSize::new(1280, 720));

    let window = Arc::new(
        event_loop.create_window(window_attributes).expect("create window"),
    );
    agg_gui::set_device_scale(window.scale_factor());

    let mut gpu = Gpu::new(Arc::clone(&window));
    let init_w = gpu.config.width as f32;
    let init_h = gpu.config.height as f32;
    let mut wgpu_ctx = WgpuGfxCtx::new(
        Arc::clone(&gpu.device),
        Arc::clone(&gpu.queue),
        gpu.surface_format,
        init_w,
        init_h,
    );

    // Build the AtomArtist UI with a starter Box visible in the viewport.
    let state = fresh_state_with_starter_graph();
    let root = build_app(state);
    let mut app = App::new(root);

    let mut win_w = gpu.config.width;
    let mut win_h = gpu.config.height;

    let mut cursor_x = 0.0f64;
    let mut cursor_y = 0.0f64;
    let mut current_mods = Modifiers::default();

    event_loop
        .run(move |event, elwt| {
            elwt.set_control_flow(ControlFlow::Wait);
            match event {
                Event::WindowEvent {
                    event: WindowEvent::CloseRequested, ..
                } => elwt.exit(),
                Event::WindowEvent {
                    event: WindowEvent::Resized(new_size), ..
                } => {
                    win_w = new_size.width;
                    win_h = new_size.height;
                    gpu.resize(win_w, win_h);
                    wgpu_ctx.reset(win_w as f32, win_h as f32);
                    window.request_redraw();
                }
                Event::WindowEvent {
                    event: WindowEvent::CursorMoved { position, .. }, ..
                } => {
                    let scale = window.scale_factor();
                    let (lx, ly) = (position.x / scale, position.y / scale);
                    // Y-flip: agg-gui is Y-up.
                    cursor_x = lx;
                    cursor_y = (win_h as f64 / scale) - ly;
                    app.on_mouse_move(cursor_x, cursor_y);
                    window.request_redraw();
                }
                Event::WindowEvent {
                    event: WindowEvent::MouseInput { state, button, .. }, ..
                } => {
                    if let Some(b) = translate_winit_button(button) {
                        match state {
                            ElementState::Pressed => app.on_mouse_down(cursor_x, cursor_y, b, current_mods),
                            ElementState::Released => app.on_mouse_up(cursor_x, cursor_y, b, current_mods),
                        }
                        window.request_redraw();
                    }
                }
                Event::WindowEvent {
                    event: WindowEvent::MouseWheel { delta, .. }, ..
                } => {
                    let dy = match delta {
                        MouseScrollDelta::LineDelta(_, y) => (y as f64) * 60.0,
                        MouseScrollDelta::PixelDelta(p) => p.y,
                    };
                    app.on_mouse_wheel(cursor_x, cursor_y, dy);
                    window.request_redraw();
                }
                Event::WindowEvent {
                    event: WindowEvent::ModifiersChanged(mods), ..
                } => {
                    let s = mods.state();
                    current_mods = Modifiers {
                        shift: s.shift_key(),
                        ctrl: s.control_key(),
                        alt: s.alt_key(),
                        meta: s.super_key(),
                    };
                }
                Event::WindowEvent {
                    event: WindowEvent::KeyboardInput { event, .. }, ..
                } => {
                    if let Some(k) = translate_winit_key(&event.logical_key) {
                        match event.state {
                            ElementState::Pressed => app.on_key_down(k, current_mods),
                            ElementState::Released => app.on_key_up(k, current_mods),
                        }
                        window.request_redraw();
                    }
                }
                Event::WindowEvent {
                    event: WindowEvent::RedrawRequested, ..
                } => {
                    paint_frame(&gpu, &mut wgpu_ctx, &mut app, win_w, win_h);
                }
                _ => {}
            }
        })
        .expect("event loop run");
}

// Phase 0 placeholder kept while atomartist-{lib,renderer,ui} stubs still
// expose `placeholder`. Removed once they all carry real public API.
#[allow(dead_code)]
fn _touch_placeholders() {
    atomartist_lib::placeholder();
    atomartist_renderer::placeholder();
    atomartist_ui::placeholder();
}
