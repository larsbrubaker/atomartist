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

use agg_gui::{
    persistence::AutoSave, App, DrawCtx, Key, Modifiers, MouseButton, text::Font,
    theme::{set_visuals, Visuals},
};
use atomartist_ui::{
    build_app, fresh_state_with_starter_graph, top_menu_bar::FileDialogProvider,
    MainWindowState, UiSettings, WindowPlacement,
};
use demo_wgpu::WgpuGfxCtx;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Event, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::window::{Window, WindowAttributes};

const DEFAULT_FONT_BYTES: &[u8] =
    include_bytes!("../../../agg-gui/agg-gui/assets/fonts/NotoSans-Regular.ttf");
const ICON_FONT_BYTES: &[u8] = include_bytes!("../../atomartist-ui/assets/bootstrap-icons.ttf");

pub(crate) struct Gpu {
    pub(crate) device: Arc<wgpu::Device>,
    pub(crate) queue: Arc<wgpu::Queue>,
    pub(crate) surface: wgpu::Surface<'static>,
    pub(crate) surface_format: wgpu::TextureFormat,
    pub(crate) config: wgpu::SurfaceConfiguration,
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

mod frame;
use frame::paint_frame;


/// Cast a winit `MonitorHandle` to a plain `(x, y, w, h)` rect in
/// physical pixels — the shape `MainWindowState::fits_on_monitors`
/// expects so the validation helper stays winit-agnostic.
fn monitor_to_rect(m: winit::monitor::MonitorHandle) -> (i32, i32, u32, u32) {
    let pos = m.position();
    let size = m.size();
    (pos.x, pos.y, size.width, size.height)
}

/// Read the current outer position + inner size + maximized state
/// from the live `Window`. Used at startup to seed the
/// "last normal bounds" cache and during persistence to capture
/// the current maximized flag.
fn current_main_window_state(window: &Window) -> MainWindowState {
    let pos = window.outer_position().unwrap_or(PhysicalPosition::new(0, 0));
    let size = window.inner_size();
    MainWindowState {
        x: pos.x,
        y: pos.y,
        width: size.width,
        height: size.height,
        maximized: window.is_maximized(),
    }
}

/// Pick the initial value for the live "last normal bounds" cache.
///
/// We always prefer the saved bounds (with the recentered position
/// if the saved one was off-screen now) over reading the live
/// window — when the shell has just called `set_maximized(true)`,
/// `outer_position()` and `inner_size()` report the maximized
/// monitor-fill geometry, which is exactly the wrong thing to seed
/// the "last non-maximized bounds" cache with. Falling back to the
/// live window is reserved for the genuine first-launch case where
/// no saved bounds exist at all.
fn initial_normal_bounds(window: &Window, saved: Option<MainWindowState>) -> MainWindowState {
    match saved {
        Some(s) if s.has_valid_geometry() => MainWindowState {
            maximized: window.is_maximized(),
            ..s
        },
        _ => current_main_window_state(window),
    }
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

/// Cross-platform "user config dir" for AtomArtist's settings file.
/// We avoid the `dirs` crate dependency by reading the well-known
/// environment variables directly:
///   - Windows: `%APPDATA%\atomartist\settings.txt`
///   - macOS: `$HOME/Library/Application Support/atomartist/settings.txt`
///   - Linux / BSD: `$XDG_CONFIG_HOME/atomartist/settings.txt`
///     or `$HOME/.config/atomartist/settings.txt`
fn settings_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|p| p.join("atomartist").join("settings.txt"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(PathBuf::from).map(|p| {
            p.join("Library")
                .join("Application Support")
                .join("atomartist")
                .join("settings.txt")
        })
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|p| p.join("atomartist").join("settings.txt"))
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

    // Load persisted UI settings up-front. We need both the HUD
    // state (applied to AppState below) AND the OS window
    // geometry (used at window creation) from the same file, so
    // do the read here before anything else looks at it. Missing
    // or unparseable file silently falls back to documented
    // defaults — never blocks startup.
    let settings_path = settings_path();
    let loaded_settings: Option<UiSettings> = settings_path
        .as_ref()
        .map(|path| UiSettings::read_from_file(path));

    // Install light theme as the default — AtomArtist is a CAD-style design
    // tool where high-contrast white backgrounds match user expectation.
    set_visuals(Visuals::light());

    let icon_font =
        Arc::new(Font::from_bytes(ICON_FONT_BYTES.to_vec()).expect("load Bootstrap Icons"));
    let font = Arc::new(
        Font::from_bytes(DEFAULT_FONT_BYTES.to_vec())
            .expect("load NotoSans-Regular")
            .with_fallback(icon_font),
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

    // Compose the initial window placement. We create the window
    // *hidden* so we can validate the saved position against the
    // attached monitors via `window.available_monitors()` (winit
    // 0.30 doesn't expose monitors on `EventLoop`) without a
    // visible "snap" from the OS-chosen position to the restored
    // one. Saved size is safe to apply up-front; only the
    // position needs runtime monitor validation.
    let saved_main = loaded_settings.as_ref().map(|s| s.main_window);
    let mut window_attributes = WindowAttributes::default()
        .with_title("AtomArtist")
        .with_visible(false);
    if let Some(w) = saved_main.filter(|w| w.has_valid_geometry()) {
        window_attributes = window_attributes
            .with_position(PhysicalPosition::new(w.x, w.y))
            .with_inner_size(PhysicalSize::new(w.width, w.height));
    } else {
        window_attributes = window_attributes.with_inner_size(LogicalSize::new(1280, 720));
    }

    let window = Arc::new(
        event_loop.create_window(window_attributes).expect("create window"),
    );
    agg_gui::set_device_scale(window.scale_factor());

    // Decide what the saved bounds map to now that we know the live
    // monitor layout. Three outcomes — see `WindowPlacement`:
    //   - Default: no usable save → keep the OS-chosen defaults.
    //   - Restore: use saved position + size + maximized as-is.
    //   - Recenter: keep saved size + maximized but pick a new
    //     centred position on the primary monitor (saved one is
    //     off-screen now).
    //
    // The maximized flag is applied unconditionally below so a user
    // who closed the app while maximized comes back to a maximized
    // window even when the un-maximized position needed adjustment.
    let placement = saved_main
        .unwrap_or_default()
        .placement(window.available_monitors().map(monitor_to_rect));
    let placement_record = match placement {
        WindowPlacement::Restore { bounds } => Some(bounds),
        WindowPlacement::Recenter { width, height, maximized } => {
            // Recentre on the primary monitor. The window already
            // has the saved size from `with_inner_size` above; only
            // the position needs fixing here.
            let recentred = window
                .available_monitors()
                .next()
                .map(|primary| {
                    let mon = primary.position();
                    let size = primary.size();
                    let cx = mon.x + (size.width as i32 - width as i32) / 2;
                    let cy = mon.y + (size.height as i32 - height as i32) / 2;
                    window.set_outer_position(PhysicalPosition::new(cx, cy));
                    (cx, cy)
                })
                .unwrap_or((0, 0));
            Some(MainWindowState {
                x: recentred.0,
                y: recentred.1,
                width,
                height,
                maximized,
            })
        }
        WindowPlacement::Default { .. } => None,
    };
    if matches!(
        placement,
        WindowPlacement::Restore { bounds: MainWindowState { maximized: true, .. } }
            | WindowPlacement::Recenter { maximized: true, .. }
            | WindowPlacement::Default { maximized: true }
    ) {
        window.set_maximized(true);
    }
    window.set_visible(true);

    // Live cache of the most recent *non-maximized* window position
    // and size — pulled from `WindowEvent::Moved/Resized` so a
    // user that maximizes mid-session still restores to the right
    // bounds on next launch. Maximized flag is sampled directly
    // off the window on save. We seed it from the placement record
    // (post-recenter) rather than `current_main_window_state`,
    // because `set_maximized(true)` above turns the live window's
    // `outer_position()` / `inner_size()` into the maximized
    // monitor-fill geometry — exactly the wrong thing for a
    // "remember last un-maximized bounds" cache.
    let normal_bounds: std::rc::Rc<std::cell::Cell<MainWindowState>> = std::rc::Rc::new(
        std::cell::Cell::new(initial_normal_bounds(&window, placement_record)),
    );
    let normal_bounds_for_save = normal_bounds.clone();
    let normal_bounds_for_events = normal_bounds.clone();
    let window_for_save = window.clone();

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
    // Apply the HUD button states (perspective / turntable / bed /
    // render style / snap) that were read from disk at the top of
    // `main`, *before* mounting the widget tree so the first paint
    // reflects what the user left things at.
    if let Some(loaded) = loaded_settings.as_ref() {
        state.apply_ui_settings(loaded);
    }
    // Auto-reopen the last project the user worked on so they
    // resume where they left off. Failure is non-fatal — the
    // starter graph stays in place and we log the reason. We do
    // this *before* mounting the widget tree so the very first
    // paint shows the restored project, not a one-frame flash of
    // the starter scene.
    if let Some(last) = loaded_settings
        .as_ref()
        .and_then(|s| s.last_project_path.as_ref())
    {
        if last.exists() {
            if let Err(e) = state.load_graph_from_path(last) {
                eprintln!(
                    "warning: could not reopen last project {}: {}",
                    last.display(),
                    e
                );
            }
        } else {
            eprintln!(
                "info: last project {} no longer exists, starting fresh",
                last.display()
            );
        }
    }
    // Clone for the persistence loop — `AppState` is `Arc`-shared
    // internally so this is just an Arc bump per field.
    let state_for_save = state.clone();
    let dialogs: std::sync::Arc<dyn FileDialogProvider> = std::sync::Arc::new(NativeDialogs);
    let (root, debug) = build_app(state, dialogs, loaded_settings);
    let mut app = App::new(root);
    // Clone for the persistence + paint loops — every field is an
    // Rc internally so this is cheap.
    let debug_for_save = debug.clone();
    let mut settings_auto_save = AutoSave::new();
    // Seed the AutoSave with whatever's currently on disk so the
    // first paint doesn't pointlessly rewrite an identical file.
    if let Some(ref path) = settings_path {
        if let Ok(existing) = std::fs::read_to_string(path) {
            settings_auto_save.seed(existing);
        }
    }
    // Track held mouse buttons so AutoSave only writes when the
    // user isn't mid-drag. Same idle guard agg-gui's persistence
    // docs recommend.
    let mut mouse_buttons_held: u32 = 0;

    let mut win_w = gpu.config.width;
    let mut win_h = gpu.config.height;
    let mut next_scheduled_redraw: Option<std::time::Instant> = None;

    let mut cursor_x = 0.0f64;
    let mut cursor_y = 0.0f64;
    let mut current_mods = Modifiers::default();

    // Screenshot mode: paint a few warmup frames so all GPU state is
    // realised, then capture + save + exit. Frame counting starts at 0.
    //
    // `ATOMARTIST_WARMUP_FRAMES` overrides the default 3 — useful for
    // diagnostic runs where you want enough samples for the periodic
    // frame-time / scene-time loggers to print.
    let mut frames_painted: u32 = 0;
    let screenshot_path = cli.screenshot_to.clone();
    let warmup_frames: u32 = std::env::var("ATOMARTIST_WARMUP_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);

    event_loop
        .run(move |event, elwt| {
            if let Some(t) = next_scheduled_redraw {
                elwt.set_control_flow(ControlFlow::WaitUntil(t));
            } else {
                elwt.set_control_flow(ControlFlow::Wait);
            }
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
                    // Cache the "user's preferred size" only when
                    // the window isn't maximized — maximizing fires
                    // a resize event with the monitor's full size,
                    // and persisting that would clobber the bounds
                    // we want to restore on un-maximize.
                    if !window.is_maximized() {
                        let mut nb = normal_bounds_for_events.get();
                        nb.width = new_size.width;
                        nb.height = new_size.height;
                        normal_bounds_for_events.set(nb);
                    }
                    window.request_redraw();
                }
                Event::WindowEvent {
                    event: WindowEvent::Moved(pos), ..
                } => {
                    if !window.is_maximized() {
                        let mut nb = normal_bounds_for_events.get();
                        nb.x = pos.x;
                        nb.y = pos.y;
                        normal_bounds_for_events.set(nb);
                    }
                }
                Event::WindowEvent {
                    event: WindowEvent::CursorMoved { position, .. }, ..
                } => {
                    // agg-gui's App::on_mouse_* expects RAW physical coords
                    // from winit (Y-down) — it handles scale + Y-flip
                    // internally via the registered device_scale and the
                    // viewport size passed to app.layout. Don't pre-convert
                    // here; that double-flips and causes hit-testing to
                    // route every event to the viewport at the bottom.
                    //
                    // DELIBERATELY no `window.request_redraw()` here.
                    // `app.on_mouse_move` runs through agg-gui's
                    // `dispatch_event`, which tracks the invalidation
                    // epoch before/after each widget's `on_event` —
                    // if any widget actually changed visible state
                    // (hover highlight, drag preview, etc.) it will
                    // call `animation::request_draw()` and the epoch
                    // bumps, which `AboutToWait` picks up via
                    // `app.wants_draw()`.  Forcing a redraw here
                    // re-paints on EVERY mouse pixel even when
                    // nothing visible changed — exactly the
                    // continuous-paint-on-cursor-move behaviour the
                    // user reported.
                    cursor_x = position.x;
                    cursor_y = position.y;
                    app.on_mouse_move(cursor_x, cursor_y);
                }
                Event::WindowEvent {
                    event: WindowEvent::MouseInput { state, button, .. }, ..
                } => {
                    if let Some(b) = translate_winit_button(button) {
                        match state {
                            ElementState::Pressed => {
                                mouse_buttons_held = mouse_buttons_held.saturating_add(1);
                                app.on_mouse_down(cursor_x, cursor_y, b, current_mods);
                            }
                            ElementState::Released => {
                                mouse_buttons_held = mouse_buttons_held.saturating_sub(1);
                                app.on_mouse_up(cursor_x, cursor_y, b, current_mods);
                            }
                        }
                        // No explicit request_redraw — the same epoch /
                        // dirty-bubble path that handles CursorMoved
                        // handles click events too.  A button widget
                        // that flips its `pressed` visual state calls
                        // request_draw from inside `on_event`; one that
                        // doesn't (e.g. a click on empty canvas
                        // background) should NOT trigger a repaint.
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
                    }
                }
                Event::WindowEvent {
                    event: WindowEvent::RedrawRequested, ..
                } => {
                    next_scheduled_redraw = None;
                    let capture_now = screenshot_path.is_some()
                        && frames_painted + 1 == warmup_frames;
                    paint_frame(&gpu, &mut wgpu_ctx, &mut app, &debug, win_w, win_h, capture_now);
                    frames_painted = frames_painted.saturating_add(1);

                    // Persist HUD button states to disk if anything
                    // changed since the last save AND the user isn't
                    // mid-drag. `AutoSave` handles the diff + idle
                    // guard so we don't write the same blob over and
                    // over, and so we never spam disk during a click.
                    if let Some(ref path) = settings_path {
                        settings_auto_save.tick(
                            mouse_buttons_held == 0,
                            || {
                                // Compose: HUD state from AppState
                                // plus floating-window layout from
                                // the live debug handles plus the
                                // OS window's last non-maximized
                                // bounds + current maximized flag.
                                // None of these own each other, so
                                // the shell stitches them together
                                // each time we render the
                                // persistence blob.
                                let mut s = state_for_save.ui_settings();
                                s.debug_windows = debug_for_save.current_state();
                                let mut main = normal_bounds_for_save.get();
                                main.maximized = window_for_save.is_maximized();
                                s.main_window = main;
                                s.to_text()
                            },
                            |blob| {
                                if let Some(parent) = path.parent() {
                                    let _ = std::fs::create_dir_all(parent);
                                }
                                if let Err(e) = std::fs::write(path, blob) {
                                    eprintln!(
                                        "warning: failed to save UI settings to {}: {}",
                                        path.display(),
                                        e
                                    );
                                }
                            },
                        );
                    }
                    // Some widgets (notably the tumble-cube click-to-orient
                    // animation) request another frame *during* paint.  Winit
                    // won't draw again unless the host explicitly asks for it,
                    // so pump agg-gui's draw flag here; otherwise animations
                    // advance one frame and then appear to resume only when the
                    // user moves the mouse.
                    let continuous =
                        debug_for_save.run_mode.get() == agg_gui::RunMode::Continuous;
                    if continuous {
                        window.request_redraw();
                    } else if agg_gui::animation::wants_draw() {
                        window.request_redraw();
                    } else if app.wants_draw() {
                        window.request_redraw();
                    } else {
                        let animation_deadline = agg_gui::animation::take_next_draw_deadline();
                        let widget_deadline = app.next_draw_deadline();
                        let next_deadline = match (animation_deadline, widget_deadline) {
                            (Some(a), Some(b)) => Some(a.min(b)),
                            (Some(a), None) => Some(a),
                            (None, Some(b)) => Some(b),
                            (None, None) => None,
                        };
                        if let Some(deadline) = next_deadline {
                            let delay = deadline.saturating_duration_since(web_time::Instant::now());
                            next_scheduled_redraw = Some(std::time::Instant::now() + delay);
                        }
                    }
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
                Event::AboutToWait => {
                    // Continuous run-mode keeps the loop spinning every
                    // frame regardless of widget invalidation — required
                    // when the Performance window's selector is flipped
                    // to "Continuous" so the FPS readout reflects a real
                    // sustained framerate, not just per-input wakeups.
                    let continuous =
                        debug_for_save.run_mode.get() == agg_gui::RunMode::Continuous;
                    if continuous
                        || agg_gui::animation::wants_draw()
                        || app.wants_draw()
                    {
                        next_scheduled_redraw = None;
                        window.request_redraw();
                    } else if let Some(deadline) = next_scheduled_redraw {
                        if std::time::Instant::now() >= deadline {
                            next_scheduled_redraw = None;
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
///
/// Filter ordering matters: rfd uses the first filter as the default
/// "Save as type" so we list `.atmr` first and keep `.json` as a
/// secondary entry for opening / converting legacy projects.
struct NativeDialogs;
impl FileDialogProvider for NativeDialogs {
    fn pick_open_project(&self) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .add_filter("AtomArtist project", &["atmr"])
            .add_filter("AtomArtist project (legacy JSON)", &["json"])
            .add_filter("All files", &["*"])
            .pick_file()
    }
    fn pick_save_project(&self, default_name: &str) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .add_filter("AtomArtist project", &["atmr"])
            .add_filter("AtomArtist project (legacy JSON)", &["json"])
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
    fn show_info(&self, title: &str, message: &str) {
        rfd::MessageDialog::new()
            .set_title(title)
            .set_description(message)
            .set_level(rfd::MessageLevel::Info)
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
