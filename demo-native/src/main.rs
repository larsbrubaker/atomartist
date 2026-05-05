//! AtomArtist native shell — winit + wgpu.
//!
//! Mounts the shared widget tree from `atomartist-ui` onto a winit window
//! using the wgpu DrawCtx from `demo-wgpu`. No application logic lives
//! here — see `atomartist-ui::build_app` for the widget tree.
//!
//! Modeled (compactly) on `agg-gui/demo-native/src/main.rs` minus the
//! inspector / screenshot / MSAA / multi-touch / font-asset machinery
//! which AtomArtist doesn't need yet.

use std::path::PathBuf;
use std::sync::Arc;

use agg_gui::{App, DrawCtx, Key, Modifiers, MouseButton, Size, text::Font, theme::{set_visuals, Visuals}};
use atomartist_ui::{build_app, fresh_state_with_starter_graph, top_menu_bar::FileDialogProvider};
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
            // COPY_SRC required for the screenshot capture path (which
            // copies the surface into an internal capture texture).
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
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

fn paint_frame(
    gpu: &Gpu,
    ctx: &mut WgpuGfxCtx,
    app: &mut App,
    w: u32,
    h: u32,
    capture_after: bool,
) {
    let frame = match gpu.surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
        _ => return,
    };
    // Stash the surface texture handle before begin_frame so the screenshot
    // path can copy from it (capture_screenshot reads ctx.surface_texture).
    ctx.set_surface_texture(frame.texture.clone());
    let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
    ctx.reset(w as f32, h as f32);
    ctx.set_lcd_mode(agg_gui::font_settings::lcd_enabled());
    begin_frame(ctx, view);
    app.layout(Size::new(w as f64, h as f64));
    app.paint(ctx);
    ctx.end_frame();
    if capture_after {
        // Must run between end_frame (commands flushed) and present
        // (surface texture destroyed). The captured pixels live inside
        // ctx.capture_texture and survive present.
        ctx.capture_screenshot();
    }
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

/// Parsed CLI: `--screenshot <path>` exits after grabbing one frame.
struct CliArgs {
    screenshot_to: Option<PathBuf>,
}

fn parse_args() -> CliArgs {
    let mut args = std::env::args().skip(1);
    let mut screenshot_to = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--screenshot" => {
                screenshot_to = args.next().map(PathBuf::from);
            }
            _ => {}
        }
    }
    CliArgs { screenshot_to }
}

#[allow(deprecated)]
fn main() {
    let cli = parse_args();
    let event_loop = EventLoop::new().expect("event loop");

    // Install light theme as the default — AtomArtist is a CAD-style design
    // tool where high-contrast white backgrounds match user expectation.
    set_visuals(Visuals::light());

    let font = Arc::new(
        Font::from_bytes(DEFAULT_FONT_BYTES.to_vec()).expect("load NotoSans-Regular"),
    );
    // Make the font available to every widget via agg-gui's thread-local
    // system-font slot, so widgets can fall back to it without an explicit
    // ctx.set_font call.
    agg_gui::font_settings::set_system_font(Some(font.clone()));

    // Text-quality recipe (mirrors agg-gui's demo):
    //   - LCD subpixel rendering + Y-axis hinting on standard-DPI displays
    //     (skip on hi-DPI to avoid colour-fringe artifacts at >1.25x).
    //   - Default gamma / width / weight / italic so the rasterizer matches
    //     the reference truetype_test demo.
    let standard_dpi = agg_gui::device_scale() <= 1.25;
    agg_gui::font_settings::set_font_size_scale(1.0);
    agg_gui::font_settings::set_lcd_enabled(standard_dpi);
    agg_gui::font_settings::set_hinting_enabled(standard_dpi);
    agg_gui::font_settings::set_gamma(1.0);
    agg_gui::font_settings::set_width(1.0);
    agg_gui::font_settings::set_interval(0.0);
    agg_gui::font_settings::set_faux_weight(0.0);
    agg_gui::font_settings::set_faux_italic(0.0);
    agg_gui::font_settings::set_primary_weight(1.0 / 3.0);

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
    let dialogs: std::sync::Arc<dyn FileDialogProvider> = std::sync::Arc::new(NativeDialogs);
    let root = build_app(state, dialogs);
    let mut app = App::new(root);

    let mut win_w = gpu.config.width;
    let mut win_h = gpu.config.height;

    let mut cursor_x = 0.0f64;
    let mut cursor_y = 0.0f64;
    let mut current_mods = Modifiers::default();

    // Screenshot mode: paint a few warmup frames so all GPU state is
    // realised, then capture + save + exit. Frame counting starts at 0.
    let mut frames_painted: u32 = 0;
    let screenshot_path = cli.screenshot_to.clone();
    let warmup_frames: u32 = 3;

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
                    let capture_now = screenshot_path.is_some()
                        && frames_painted + 1 == warmup_frames;
                    paint_frame(&gpu, &mut wgpu_ctx, &mut app, win_w, win_h, capture_now);
                    frames_painted = frames_painted.saturating_add(1);
                    if let Some(path) = screenshot_path.clone() {
                        if frames_painted == warmup_frames {
                            // Capture happened above; pixels are now in the
                            // capture texture. Read them back and exit.
                            let (pixels, w, h) = wgpu_ctx.read_captured_screenshot();
                            if !pixels.is_empty() && w > 0 && h > 0 {
                                if let Err(e) = save_rgba_png(&path, &pixels, w, h) {
                                    eprintln!("screenshot write failed: {}", e);
                                } else {
                                    eprintln!("wrote {}x{} screenshot to {}", w, h, path.display());
                                }
                            } else {
                                eprintln!("screenshot capture returned no pixels");
                            }
                            elwt.exit();
                        } else {
                            window.request_redraw();
                        }
                    }
                }
                _ => {}
            }
        })
        .expect("event loop run");
}

/// File-dialog provider for native — backed by `rfd`. Blocking dialogs
/// are fine: the agg-gui App's render loop is paused while the modal is
/// up, and the user's response unblocks it.
struct NativeDialogs;
impl FileDialogProvider for NativeDialogs {
    fn pick_open_project(&self) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .add_filter("AtomArtist project", &["json"])
            .pick_file()
    }
    fn pick_save_project(&self, default_name: &str) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .add_filter("AtomArtist project", &["json"])
            .set_file_name(default_name)
            .save_file()
    }
    fn pick_save_stl(&self, default_name: &str) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .add_filter("Binary STL", &["stl"])
            .set_file_name(default_name)
            .save_file()
    }
    fn show_error(&self, message: &str) {
        rfd::MessageDialog::new()
            .set_title("AtomArtist")
            .set_description(message)
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
}

/// Encode an RGBA8 buffer to PNG. The capture path returns Y-down rows
/// (wgpu surface convention), which matches PNG's natural top-down order
/// — no flip needed.
fn save_rgba_png(path: &std::path::Path, pixels: &[u8], w: u32, h: u32) -> Result<(), String> {
    use image::ImageBuffer;
    let buf = ImageBuffer::<image::Rgba<u8>, &[u8]>::from_raw(w, h, pixels)
        .ok_or_else(|| format!("image buffer build failed: pixels={} w={} h={}", pixels.len(), w, h))?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    buf.save(path).map_err(|e| e.to_string())
}

// Phase 0 placeholder kept while atomartist-{lib,renderer,ui} stubs still
// expose `placeholder`. Removed once they all carry real public API.
#[allow(dead_code)]
fn _touch_placeholders() {
    atomartist_lib::placeholder();
    atomartist_renderer::placeholder();
    atomartist_ui::placeholder();
}
