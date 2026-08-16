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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use agg_gui::{persistence::AutoSave, App, DrawCtx, Modifiers};
use atomartist_storage::{LocalFsProvider, StorageRegistry};
use atomartist_ui::{
    build_app, fresh_state_with_starter_graph_and_storage, install_theme_and_fonts,
    top_menu_bar::FileDialogProvider, MainWindowState, UiSettings, WindowPlacement,
};
use demo_wgpu::WgpuGfxCtx;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Event, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::window::WindowAttributes;

mod close_gate;
mod dialogs;
mod frame;
mod gpu;
mod shell_settings;
mod thumbnail_capture;
mod wake;
mod winit_input;

use close_gate::{deferred_close_decision, DeferredClose};
use dialogs::NativeDialogs;
use frame::paint_frame;
use gpu::Gpu;
use shell_settings::{
    compose_settings_blob, initial_normal_bounds, monitor_to_rect, settings_path,
    write_settings_blob,
};
use winit_input::{live_cursor_in_window, translate_winit_button, translate_winit_key};

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
    // Let anything that finishes off the main thread wake this loop out
    // of `ControlFlow::Wait`. Without it the storage pump would have to
    // keep asking for frames it does not need — see `wake`. Installed
    // before anything can submit work (the last-project reopen below).
    //
    // Two links in one chain: a settling `Job` calls the storage
    // completion hook, which signals agg-gui, which calls the host waker
    // installed here, which pushes an event at this loop.
    atomartist_ui::install_storage_wakeups();
    wake::install_host_waker(event_loop.create_proxy());
    let mut frame_probe = wake::FrameRateProbe::new();

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

    // Theme + fonts + text-quality are installed *after* window creation
    // (see below), once `window.scale_factor()` is known — the LCD/hinting
    // DPI decision needs the real device scale. The recipe itself lives in
    // `atomartist_ui::install_theme_and_fonts`, shared verbatim with the
    // wasm shell so the two render pixel-identically.

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
    let device_scale = window.scale_factor();
    agg_gui::set_device_scale(device_scale);
    install_theme_and_fonts(device_scale);

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

    // Storage backends this shell offers. Native gets the real
    // filesystem under the `file:` scheme; `atomartist-ui` itself
    // registers nothing, so the choice lives here in the shell.
    let storage = {
        let mut registry = StorageRegistry::new();
        registry
            .register(Arc::new(LocalFsProvider::new()))
            .expect("fresh registry accepts the local filesystem provider");
        Arc::new(registry)
    };

    // Build the AtomArtist UI with a starter Box visible in the viewport.
    let state = fresh_state_with_starter_graph_and_storage(storage);
    // Apply the HUD button states (perspective / turntable / bed /
    // render style / snap) that were read from disk at the top of
    // `main`, *before* mounting the widget tree so the first paint
    // reflects what the user left things at.
    if let Some(loaded) = loaded_settings.as_ref() {
        state.apply_ui_settings(loaded);
    }
    // Auto-reopen the last project the user worked on so they
    // resume where they left off. Failure is non-fatal AND not an
    // error: nobody asked for this open, so a project deleted since
    // the last session is news, not a fault — `reopen_last_project`
    // reports it at Info level and prunes the recent entry. (An
    // error notice would be sticky, sitting in the status bar's one
    // slot suppressing the user's first "Saved …".) No separate
    // existence pre-check: a deleted file, or one written by a
    // backend this build no longer registers, fails the read with
    // exactly the message we would have printed. We submit *before*
    // mounting the widget tree so the very first paint shows the
    // restored project, not a one-frame flash of the starter scene
    // — the `file:` provider settles its job inline, so the open is
    // already applied when this returns.
    if let Some(last) = loaded_settings
        .as_ref()
        .and_then(|s| s.last_project_path.as_ref())
    {
        state.reopen_last_project(last);
    }
    // Clone for the persistence loop — `AppState` is `Arc`-shared
    // internally so this is just an Arc bump per field.
    let state_for_save = state.clone();
    let dialogs: std::sync::Arc<dyn FileDialogProvider> = std::sync::Arc::new(NativeDialogs);
    // The close path needs the same provider the widget tree uses, and
    // needs it as an `Arc`: a failed save-on-close raises its modal from
    // the write's continuation, which outlives this event.
    let dialogs_for_close = dialogs.clone();
    // Handle to the in-app Open/Save picker. Built here (not inside the
    // tree) because step 6c-2's dialog provider will hold a clone; today
    // nothing opens it yet.
    let browser_modal = atomartist_ui::file_browser::FileBrowserModalHandle::new();
    let (root, debug) = build_app(state, dialogs, loaded_settings, browser_modal);
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
    // Set by the close prompt's Save continuation: "the save the user
    // asked for is confirmed, you may now shut down". An `AtomicBool`
    // rather than a `Cell` because the continuation is a `Send` closure
    // owned by the pump.
    let close_when_idle = Arc::new(AtomicBool::new(false));
    let screenshot_path = cli.screenshot_to.clone();
    // Opportunistic project-preview capture (see `thumbnail_capture`).
    // Disabled in `--screenshot` mode: that run owns the capture texture
    // and exits after a handful of frames.
    let mut thumbs = thumbnail_capture::ThumbnailCapture::new(screenshot_path.is_none());
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
                // The host waker's nudge (`wake::install_host_waker`).
                // Carries no payload and needs no handler: its whole job
                // is to get us here, and the `AboutToWait` that always
                // follows a delivered event runs the storage pump — which
                // reads `wants_draw()`, merging the cross-thread wakeup
                // the signaller published before nudging us.
                Event::UserEvent(()) => {}
                Event::WindowEvent {
                    event: WindowEvent::CloseRequested, ..
                } => {
                    // Unsaved-changes gate: same Save / Discard / Cancel
                    // flow the File menu's destructive actions use.
                    // Skipped in screenshot mode — those runs are
                    // headless and must never block on a dialog.
                    //
                    // The gate is asynchronous now: choosing **Save**
                    // submits the write and the permission to close
                    // arrives in that write's continuation, so we set a
                    // flag there instead of returning a verdict. With the
                    // `file:` provider the job is already settled when
                    // `submit_op` sees it, the continuation runs inline,
                    // and the flag is set before the call below returns —
                    // the window still closes on this very event. With a
                    // slower provider we stay open and the `AboutToWait`
                    // arm finishes the close once the pump delivers the
                    // result; a failed save leaves the flag clear, the
                    // window open, and the error notice on screen.
                    if screenshot_path.is_none() && !close_when_idle.load(Ordering::SeqCst) {
                        let flag = close_when_idle.clone();
                        atomartist_ui::menu_actions::confirm_discard_unsaved_then(
                            &state_for_save,
                            &dialogs_for_close,
                            move |_state| flag.store(true, Ordering::SeqCst),
                        );
                        if !close_when_idle.load(Ordering::SeqCst) {
                            return;
                        }
                    }
                    // Flush pending settings before exiting so the
                    // last-opened project path (and theme / accent /
                    // window bounds the user just changed) survives
                    // even when the close happens between paints —
                    // a native modal dialog can leave AutoSave with
                    // a non-zero `mouse_buttons_held` count that
                    // would otherwise skip the final write.
                    if let Some(ref path) = settings_path {
                        let blob = compose_settings_blob(
                            &state_for_save,
                            &debug_for_save,
                            &normal_bounds_for_save,
                            &window_for_save,
                        );
                        write_settings_blob(path, &blob);
                    }
                    // Last chance for in-flight storage work: the event
                    // loop is about to stop calling `pump_storage`, so a
                    // save whose job has not settled yet would be lost
                    // without a word. Bounded so a wedged provider can't
                    // hang the close; on timeout the remaining ops are
                    // cancelled, pumped once so their continuations
                    // observe `Cancelled`, and named on stderr.
                    state_for_save.drain_pending_ops(std::time::Duration::from_secs(5));
                    elwt.exit();
                }
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
                    event: WindowEvent::CursorLeft { .. }, ..
                } => {
                    // The cursor left the window, so no further
                    // `CursorMoved` arrives to clear hover state. Without
                    // this, a widget that latches a hover flag (the
                    // favourites bar's grip, for one) keeps its highlight
                    // — and its tooltip — after a fast flick out of the
                    // window. `on_mouse_leave` re-dispatches the (-1, -1)
                    // sentinel move that every hover hit-test already
                    // reads as "outside", and resets the cursor icon.
                    //
                    // Same no-explicit-redraw rule as `CursorMoved`: the
                    // widget's own `request_draw` bumps the epoch when
                    // something visible actually changed.
                    app.on_mouse_leave();
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
                    event: WindowEvent::DroppedFile(path), ..
                } => {
                    // winit emits one DroppedFile per file in a multi-file
                    // drop. Forward each separately at the drop position.
                    //
                    // (cursor_x, cursor_y) is STALE here on Windows: the
                    // OS owns the pointer during an OLE drag, winit emits
                    // no CursorMoved for it, and its IDropTarget::Drop
                    // discards the drop point — so the tracked cursor
                    // still says wherever the mouse was before the drag
                    // began. Query the live cursor instead; fall back to
                    // the tracked position on other platforms.
                    let (drop_x, drop_y) =
                        live_cursor_in_window(&window).unwrap_or((cursor_x, cursor_y));
                    app.on_file_dropped(drop_x, drop_y, vec![path]);
                }
                Event::WindowEvent {
                    event: WindowEvent::MouseWheel { delta, .. }, ..
                } => {
                    // agg-gui's wheel delta is in **notches**, not pixels:
                    // every consumer scales it itself (`ScrollView` by
                    // 40 px, the favorites strip by `SCROLL_STEP`, the
                    // browser grid by `GRID_SCROLL_STEP`), and agg-gui's
                    // own shell passes `LineDelta` straight through and
                    // divides `PixelDelta` by 40. Handing them a pixel
                    // delta instead multiplied every scroll by ~60 — the
                    // "scroll steps are far too large" report (design
                    // §5c, step 6g-2). Zoom consumers read only the sign
                    // and are unaffected.
                    let dy = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y as f64,
                        MouseScrollDelta::PixelDelta(p) => p.y / 40.0,
                    };
                    // `_xy_mods` rather than the 2-arg `on_mouse_wheel`:
                    // that wrapper hardcodes `Modifiers::default()`, so
                    // Ctrl+wheel zoom / Shift+wheel horizontal scroll
                    // never reached the widgets. `current_mods` is
                    // already tracked from `ModifiersChanged` below.
                    // delta_x is 0 — winit's horizontal wheel component
                    // isn't plumbed through here yet.
                    app.on_mouse_wheel_xy_mods(cursor_x, cursor_y, 0.0, dy, current_mods);
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
                    paint_frame(
                        &gpu, &mut wgpu_ctx, &mut app, &debug, win_w, win_h, capture_now,
                        &mut thumbs, &state_for_save,
                    );
                    frames_painted = frames_painted.saturating_add(1);
                    frame_probe.frame();

                    // Persist HUD button states to disk if anything
                    // changed since the last save AND the user isn't
                    // mid-drag. `AutoSave` handles the diff + idle
                    // guard so we don't write the same blob over and
                    // over, and so we never spam disk during a click.
                    if let Some(ref path) = settings_path {
                        settings_auto_save.tick(
                            mouse_buttons_held == 0,
                            || {
                                compose_settings_blob(
                                    &state_for_save,
                                    &debug_for_save,
                                    &normal_bounds_for_save,
                                    &window_for_save,
                                )
                            },
                            |blob| write_settings_blob(path, blob),
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
                        let animation_deadline = agg_gui::animation::peek_next_draw_deadline();
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
                            // Deliberately no `drain_pending_ops` here:
                            // `--screenshot-to` is a headless capture run
                            // that never touches a user's document, so
                            // there is no in-flight save worth waiting on
                            // — and waiting would only slow the harness.
                            elwt.exit();
                        } else {
                            window.request_redraw();
                        }
                    }
                }
                Event::AboutToWait => {
                    // Storage job pump, ahead of the paint decision below:
                    // a job that settled since the last frame must be
                    // applied even on a frame that ends up painting
                    // nothing. The pump decides for itself whether what
                    // is queued is worth another frame (an advancing
                    // percentage) or only a slow re-check
                    // (`atomartist_ui::storage_wakeup`); anything that
                    // settles off-thread wakes us through the host waker
                    // installed at startup instead.
                    state_for_save.pump_storage();
                    frame_probe.wakeup(|| state_for_save.pending_op_count_all());

                    // Deferred close: the user answered "Save" to the
                    // close prompt and that save has now landed (the
                    // pump above ran its continuation, which set the
                    // flag). Re-validate before acting on it — the
                    // permission was given for the document as it stood
                    // at the click, and the user may have kept editing
                    // while the write was in flight. See `close_gate`.
                    match deferred_close_decision(
                        close_when_idle.load(Ordering::SeqCst),
                        state_for_save.has_unsaved_changes(),
                    ) {
                        DeferredClose::NotRequested => {}
                        DeferredClose::CancelledByNewEdits => {
                            close_when_idle.store(false, Ordering::SeqCst);
                            state_for_save.notify(
                                atomartist_ui::NoticeLevel::Info,
                                "Close cancelled — there are unsaved changes made \
                                 since you chose Save.",
                            );
                        }
                        DeferredClose::Close => {
                            if let Some(ref path) = settings_path {
                                let blob = compose_settings_blob(
                                    &state_for_save,
                                    &debug_for_save,
                                    &normal_bounds_for_save,
                                    &window_for_save,
                                );
                                write_settings_blob(path, &blob);
                            }
                            state_for_save
                                .drain_pending_ops(std::time::Duration::from_secs(5));
                            elwt.exit();
                            return;
                        }
                    }

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
                    } else {
                        // Nothing wants a frame *now*; the only question
                        // left is whether a scheduled one is due or
                        // pending. See `wake::next_turn` for why the two
                        // deadline sources are merged rather than chained.
                        match wake::next_turn(next_scheduled_redraw) {
                            wake::NextTurn::Now => {
                                next_scheduled_redraw = None;
                                window.request_redraw();
                            }
                            // The control flow is applied here rather than
                            // left to the top of the next iteration: that
                            // decision was already made for the sleep we
                            // are about to enter, so deferring it would
                            // park us in `Wait` and lose the wake-up.
                            wake::NextTurn::Until(when) => {
                                next_scheduled_redraw = Some(when);
                                elwt.set_control_flow(ControlFlow::WaitUntil(when));
                            }
                            wake::NextTurn::Indefinitely => {}
                        }
                    }
                }
                _ => {}
            }
        })
        .expect("event loop run");

    // Nothing is left to wake: drop both links of the wakeup chain so a
    // late worker thread signals into nothing (and so the retained
    // `EventLoopProxy` goes away with the waker).
    wake::clear_host_waker();
    atomartist_ui::clear_storage_wakeups();
}


// Phase 0 placeholder kept while atomartist-{lib,renderer,ui} stubs still
// expose `placeholder`. Removed once they all carry real public API.
#[allow(dead_code)]
fn _touch_placeholders() {
    atomartist_lib::placeholder();
    atomartist_renderer::placeholder();
    atomartist_ui::placeholder();
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
