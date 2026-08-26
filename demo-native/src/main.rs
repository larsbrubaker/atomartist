//! AtomArtist native shell — a thin shim over `agg-gui-shell`.
//!
//! The window, event loop, input mapping, wgpu present, surface / device-loss
//! recovery and the DPI-safe window-size restore are `agg-gui-shell`'s. What
//! is left here is genuinely AtomArtist's, threaded back in through the
//! shell's hooks (see `host`):
//!
//! - storage backends and the persisted-settings file (`shell_settings`),
//! - the window *position* restore + off-screen recenter (the shell persists
//!   only size/maximized; x/y and monitor validation are app glue below),
//! - the unsaved-changes close gate (`host::AtomHost::on_close_requested` +
//!   `close_gate`),
//! - the project-preview thumbnail capture (`thumbnail_capture`),
//! - `--screenshot <path>` runs, mapped onto the shell's deterministic
//!   capture (`ATOMARTIST_WARMUP_FRAMES` overrides the settle-frame count).
//!
//! No application logic lives here — see `atomartist-ui::build_app` for the
//! widget tree.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use agg_gui_shell::winit::dpi::PhysicalPosition;
use agg_gui_shell::winit::window::Window;
use agg_gui_shell::{CopySrc, ShellConfig, ShellError};
use atomartist_storage::{LocalFsProvider, StorageRegistry};
use atomartist_ui::{
    build_app, fresh_state_with_starter_graph_and_storage, install_theme_and_fonts,
    top_menu_bar::FileDialogProvider, MainWindowState, UiSettings, WindowPlacement,
};

mod close_gate;
mod dialogs;
mod frame;
mod host;
mod probe;
mod shell_settings;
mod thumbnail_capture;

use dialogs::NativeDialogs;
use host::{install_uncaptured_error_handler, AtomHost, SettingsBounds};
use shell_settings::{initial_normal_bounds, monitor_to_rect, settings_path};

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

fn main() -> Result<(), ShellError> {
    let cli = parse_args();

    // Let anything that finishes off the main thread wake the loop out of
    // its parked state. Two links in one chain: a settling `Job` calls the
    // storage completion hook, which signals agg-gui, which calls the host
    // waker the shell installs over its winit proxy. Installed before
    // anything can submit work (the last-project reopen below).
    atomartist_ui::install_storage_wakeups();

    // Load persisted UI settings up-front. We need both the HUD state
    // (applied to AppState below) AND the OS window geometry (fed to the
    // shell's bounds restore and the position glue) from the same file.
    // Missing or unparseable file silently falls back to documented
    // defaults — never blocks startup.
    let settings_path = settings_path();
    let loaded_settings: Option<UiSettings> = settings_path
        .as_ref()
        .map(|path| UiSettings::read_from_file(path));
    let saved_main = loaded_settings.as_ref().map(|s| s.main_window);

    // Live cache of the most recent *non-maximized* window position and
    // size, seeded in the builder once the window exists; the settings
    // blob reads it so a user who maximizes mid-session still restores to
    // the right bounds next launch.
    let normal_bounds: Rc<std::cell::Cell<MainWindowState>> = Rc::new(Default::default());

    // Screenshot mode: the shell paints settle frames, captures, writes the
    // PNG and leaves the loop. `ATOMARTIST_WARMUP_FRAMES` overrides the
    // default 3 — useful for diagnostic runs that want enough samples for
    // the periodic frame-time loggers to print.
    let warmup_frames: u32 = std::env::var("ATOMARTIST_WARMUP_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);

    let mut config = ShellConfig::new("AtomArtist")
        .with_logical_size(1280.0, 720.0)
        // NOT AutoVsync (FIFO): on Windows a reactive redraw loop paired
        // with FIFO makes `get_current_texture()` block on the DWM present
        // queue for many vblank intervals. AutoNoVsync resolves to
        // Immediate/Mailbox so each frame presents without pacing to the
        // refresh rate; a CAD viewport tolerates the occasional tear for
        // the latency win.
        .with_present_mode(agg_gui_shell::wgpu::PresentMode::AutoNoVsync)
        // The project-preview thumbnail (and the screenshot path) copy the
        // live framebuffer; a surface that can't offer COPY_SRC should cost
        // us the preview, not the whole app.
        .with_copy_src(CopySrc::IfSupported)
        // 32-bit-float blending when the adapter offers it: the dual-peel
        // chain stores depth in a blendable float target, and half-float
        // can't separate perspective-compressed transparent layers. Masked
        // against the adapter, so the app still starts without it (falling
        // back to half-float depth).
        .with_optional_features(agg_gui_shell::wgpu::Features::FLOAT32_BLENDABLE)
        .with_device_label("atomartist-native-wgpu")
        .with_bounds_store(SettingsBounds {
            loaded: saved_main,
            normal_bounds: Rc::clone(&normal_bounds),
        });
    if let Some(ref path) = cli.screenshot_to {
        config = config.with_screenshot(path.clone(), warmup_frames);
    }

    let screenshot_path = cli.screenshot_to.clone();
    let result = agg_gui_shell::run(config, move |init| {
        let window = Arc::clone(init.window());
        install_uncaptured_error_handler(init.gpu().device());

        // The shell restored the window *size*; position is ours. Validate
        // the saved position against the attached monitors and either
        // restore it, or recenter on the primary when it is off-screen now.
        // The window is still hidden here, so nothing snaps visibly.
        let placement_record = apply_window_position(&window, saved_main);
        normal_bounds.set(initial_normal_bounds(&window, placement_record));

        // Theme + fonts + text-quality, now that the real device scale is
        // known (the LCD/hinting DPI decision needs it). The recipe lives
        // in `atomartist_ui`, shared verbatim with the wasm shell so the
        // two render pixel-identically.
        install_theme_and_fonts(init.device_scale());

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

        // Build the AtomArtist UI with a starter Box visible in the
        // viewport, applying the HUD button states read from disk *before*
        // mounting the widget tree so the first paint reflects what the
        // user left things at.
        let state = fresh_state_with_starter_graph_and_storage(storage);
        if let Some(loaded) = loaded_settings.as_ref() {
            state.apply_ui_settings(loaded);
        }
        // Auto-reopen the last project the user worked on. Failure is
        // non-fatal AND not an error — see `AppState::reopen_last_project`.
        // Submitted before mounting the widget tree so the very first paint
        // shows the restored project (the `file:` provider settles inline).
        if let Some(last) = loaded_settings
            .as_ref()
            .and_then(|s| s.last_project_path.as_ref())
        {
            state.reopen_last_project(last);
        }

        let dialogs: Arc<dyn FileDialogProvider> = Arc::new(NativeDialogs);
        // Handle to the in-app Open/Save picker. Built here (not inside the
        // tree) because step 6c-2's dialog provider will hold a clone.
        let browser_modal = atomartist_ui::file_browser::FileBrowserModalHandle::new();
        let (root, debug) =
            build_app(state.clone(), dialogs.clone(), loaded_settings, browser_modal);
        let app = agg_gui::App::new(root);

        let mut settings_auto_save = agg_gui::persistence::AutoSave::new();
        // Seed with whatever's currently on disk so the first paint doesn't
        // pointlessly rewrite an identical file.
        if let Some(ref path) = settings_path {
            if let Ok(existing) = std::fs::read_to_string(path) {
                settings_auto_save.seed(existing);
            }
        }

        let host = AtomHost {
            window,
            state,
            debug,
            dialogs,
            settings_path,
            settings_auto_save,
            normal_bounds: Rc::clone(&normal_bounds),
            close_when_idle: Arc::new(AtomicBool::new(false)),
            screenshot_mode: screenshot_path.is_some(),
            // Opportunistic project-preview capture, disabled in
            // `--screenshot` mode: that run owns the capture texture and
            // exits after a handful of frames.
            thumbs: thumbnail_capture::ThumbnailCapture::new(screenshot_path.is_none()),
            frame_probe: probe::FrameRateProbe::new(),
            frame_log: frame::FrameLog::new(),
        };
        Ok((app, host))
    });

    // Drop the storage half of the wakeup chain so a late worker thread
    // signals into nothing (the shell already cleared its host waker).
    atomartist_ui::clear_storage_wakeups();

    if result.is_ok() {
        if let Some(path) = cli.screenshot_to {
            eprintln!("wrote screenshot to {}", path.display());
        }
    }
    result
}

/// Position half of the window restore. Decide what the saved bounds map to
/// now that the live monitor layout is known — see `WindowPlacement`:
///
/// - `Default`: no usable save → keep the OS-chosen position (but still
///   honour a saved maximized flag, which the shell's bounds store dropped
///   along with the invalid geometry).
/// - `Restore`: move to the saved position.
/// - `Recenter`: keep saved size + maximized but pick a new centred position
///   on the primary monitor (the saved one is off-screen now).
///
/// Returns the record to seed the "last normal bounds" cache with, `None`
/// for the genuine first-launch case.
fn apply_window_position(
    window: &Window,
    saved: Option<MainWindowState>,
) -> Option<MainWindowState> {
    let placement = saved
        .unwrap_or_default()
        .placement(window.available_monitors().map(monitor_to_rect));
    // Setting the outer position of a maximized window would disturb its
    // monitor-fill geometry, so un-maximize around the move. The window is
    // still hidden, so nothing flashes.
    let move_to = |x: i32, y: i32| {
        let was_maximized = window.is_maximized();
        if was_maximized {
            window.set_maximized(false);
        }
        window.set_outer_position(PhysicalPosition::new(x, y));
        if was_maximized {
            window.set_maximized(true);
        }
    };
    match placement {
        WindowPlacement::Restore { bounds } => {
            move_to(bounds.x, bounds.y);
            Some(bounds)
        }
        WindowPlacement::Recenter {
            width,
            height,
            maximized,
        } => {
            let recentred = window
                .available_monitors()
                .next()
                .map(|primary| {
                    let mon = primary.position();
                    let size = primary.size();
                    let cx = mon.x + (size.width as i32 - width as i32) / 2;
                    let cy = mon.y + (size.height as i32 - height as i32) / 2;
                    move_to(cx, cy);
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
        WindowPlacement::Default { maximized } => {
            // The saved geometry was unusable, but a maximized flag is
            // still honoured — a user who closed the app while maximized
            // comes back to a maximized window.
            if maximized && !window.is_maximized() {
                window.set_maximized(true);
            }
            None
        }
    }
}

// Phase 0 placeholder kept while atomartist-{lib,renderer,ui} stubs still
// expose `placeholder`. Removed once they all carry real public API.
#[allow(dead_code)]
fn _touch_placeholders() {
    atomartist_lib::placeholder();
    atomartist_renderer::placeholder();
    atomartist_ui::placeholder();
}
