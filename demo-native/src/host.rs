//! AtomArtist's [`ShellHost`] — everything `agg-gui-shell` does not own.
//!
//! The shell runs the window, event loop, input, present, device-loss
//! recovery and window-bounds persistence; this host threads back in what is
//! genuinely AtomArtist's:
//!
//! - the inspector edit/snapshot wiring around layout + paint (`frame`),
//! - the storage job pump and the deferred window-close handshake
//!   (`close_gate`) on the idle tick,
//! - the unsaved-changes gate on the OS close button
//!   ([`ShellHost::on_close_requested`]),
//! - settings persistence (`shell_settings` + `AutoSave`), including the
//!   window *position*, which the shell's [`SavedBounds`] does not carry —
//!   x/y are sampled off the live window on the idle tick instead of from a
//!   `Moved` event (the shell exposes no move hook),
//! - the opportunistic project-preview capture (`thumbnail_capture`), split
//!   across `paint` (crop rect from the just-laid-out tree) and
//!   `after_paint` (the post-`end_frame`, pre-`present` GPU window),
//! - the run-mode → redraw-policy mirror and the `ATOMARTIST_FPS_LOG` probe.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use agg_gui::persistence::AutoSave;
use agg_gui::App;
use agg_gui_shell::winit::window::Window;
use agg_gui_shell::{
    Frame, Gpu, SavedBounds, ShellControl, ShellHost, WgpuGfxCtx, WindowBoundsStore,
};
use atomartist_ui::top_menu_bar::FileDialogProvider;
use atomartist_ui::{AppState, DebugWindowHandles, MainWindowState};

use crate::close_gate::{deferred_close_decision, DeferredClose};
use crate::frame::FrameLog;
use crate::probe::FrameRateProbe;
use crate::shell_settings::{compose_settings_blob, write_settings_blob};
use crate::thumbnail_capture::ThumbnailCapture;

/// Route uncaptured wgpu errors to stderr rather than letting them vanish.
/// Without a handler a failed texture allocation or rejected shader module
/// just produces an empty frame, which reads as a renderer bug rather than a
/// resource problem. Installed at startup and again after device-loss
/// recovery (the handler dies with the old device).
pub fn install_uncaptured_error_handler(device: &wgpu::Device) {
    device.on_uncaptured_error(std::sync::Arc::new(|e: wgpu::Error| {
        eprintln!("wgpu uncaptured error: {e}");
    }));
}

/// Window width/height/maximized inside AtomArtist's own settings file.
///
/// `load` feeds the shell's DPI-safe window restore; `save` only records the
/// windowed size into the shared "last normal bounds" cell — the file itself
/// is written by the settings `AutoSave` tick (and the exit flush), which
/// serializes the window geometry together with the rest of the UI settings.
/// The x/y position is not the store's business (the shell doesn't track
/// it); [`AtomHost::track_normal_bounds`] samples it off the live window.
pub struct SettingsBounds {
    pub loaded: Option<MainWindowState>,
    pub normal_bounds: Rc<std::cell::Cell<MainWindowState>>,
}

impl WindowBoundsStore for SettingsBounds {
    fn load(&self) -> Option<SavedBounds> {
        self.loaded.filter(|w| w.has_valid_geometry()).map(|w| SavedBounds {
            width: w.width,
            height: w.height,
            maximized: w.maximized,
        })
    }

    fn save(&self, bounds: SavedBounds) {
        // The shell hands us the last *windowed* size (never the maximized
        // rect). Record it; maximized is sampled live at compose time.
        let mut nb = self.normal_bounds.get();
        nb.width = bounds.width;
        nb.height = bounds.height;
        self.normal_bounds.set(nb);
    }
}

pub struct AtomHost {
    pub window: Arc<Window>,
    pub state: AppState,
    pub debug: DebugWindowHandles,
    /// Same provider the widget tree uses; the close gate raises the same
    /// Save / Discard / Cancel modal the File menu's destructive actions do.
    pub dialogs: Arc<dyn FileDialogProvider>,
    pub settings_path: Option<PathBuf>,
    pub settings_auto_save: AutoSave,
    /// Most recent *non-maximized* window bounds (position and size) — what
    /// the settings file records, so a maximized session still restores to
    /// the right windowed geometry next launch.
    pub normal_bounds: Rc<std::cell::Cell<MainWindowState>>,
    /// Set by the close prompt's Save continuation: "the save the user asked
    /// for is confirmed, you may now shut down". See `close_gate`.
    pub close_when_idle: Arc<AtomicBool>,
    /// A `--screenshot` run: headless, never blocks on a dialog, writes no
    /// settings, and skips the drain (nothing user-owned is in flight).
    pub screenshot_mode: bool,
    pub thumbs: ThumbnailCapture,
    pub frame_probe: FrameRateProbe,
    pub frame_log: FrameLog,
}

impl AtomHost {
    fn flush_settings(&self) {
        if let Some(ref path) = self.settings_path {
            let blob = compose_settings_blob(
                &self.state,
                &self.debug,
                &self.normal_bounds,
                &self.window,
            );
            write_settings_blob(path, &blob);
        }
    }

    /// Sample the live window into the "last normal bounds" cache. Replaces
    /// the old shell's `Moved`/`Resized` event tracking — the shell exposes
    /// no move hook, but every OS move/resize wakes the loop, so the idle
    /// tick sees each change. Skipped while maximized or fullscreen: those
    /// report monitor-fill geometry, exactly what must not be persisted as
    /// the windowed bounds.
    fn track_normal_bounds(&self) {
        if self.window.is_maximized() || self.window.fullscreen().is_some() {
            return;
        }
        let mut nb = self.normal_bounds.get();
        if let Ok(pos) = self.window.outer_position() {
            nb.x = pos.x;
            nb.y = pos.y;
        }
        let size = self.window.inner_size();
        if size.width > 0 && size.height > 0 {
            nb.width = size.width;
            nb.height = size.height;
        }
        self.normal_bounds.set(nb);
    }
}

impl ShellHost for AtomHost {
    fn on_frame(&mut self, _app: &mut App, frame: &Frame) {
        self.frame_probe.frame();
        // The Performance graph plots the full wall-clock cost of the
        // previous frame (paint + submit + present) — the only number a user
        // can correlate with perceived smoothness. Zero for the first frame.
        self.debug
            .frame_history
            .borrow_mut()
            .push(frame.duration.as_secs_f32() * 1000.0);
    }

    fn paint(&mut self, app: &mut App, ctx: &mut WgpuGfxCtx, frame: &Frame) {
        crate::frame::paint_app_frame(
            app,
            ctx,
            &self.debug,
            frame,
            &mut self.frame_log,
        );
        // Read the preview crop from the tree we *just* laid out, so the
        // rectangle handed to the GPU in `after_paint` matches this exact
        // frame.
        self.thumbs.note_frame(app, frame.width, frame.height);
    }

    fn after_paint(&mut self, ctx: &mut WgpuGfxCtx, frame: &Frame) {
        self.frame_log.record_end_frame(ctx, frame);
        // Post-`end_frame`, pre-`present`: the only window where the surface
        // texture holds this frame's finished image and still exists.
        self.thumbs.tick(ctx, &self.state);
    }

    fn on_close_requested(&mut self, _app: &mut App) -> bool {
        // Headless capture runs must never block on a dialog.
        if self.screenshot_mode {
            return true;
        }
        if !self.close_when_idle.load(Ordering::SeqCst) {
            // Unsaved-changes gate: same Save / Discard / Cancel flow the
            // File menu's destructive actions use. Choosing **Save** submits
            // the write and the permission to close arrives in that write's
            // continuation. With the `file:` provider the job settles
            // inline, so the flag is set before this call returns and the
            // window closes on this very event; with a slower provider we
            // return `false` (stay open) and `on_idle`'s deferred-close arm
            // finishes the job once the pump delivers the result. Cancel and
            // a failed save leave the flag clear and the window open.
            let flag = self.close_when_idle.clone();
            atomartist_ui::menu_actions::confirm_discard_unsaved_then(
                &self.state,
                &self.dialogs,
                move |_state| flag.store(true, Ordering::SeqCst),
            );
            if !self.close_when_idle.load(Ordering::SeqCst) {
                return false;
            }
        }
        true
    }

    fn on_idle(&mut self, _app: &mut App, control: &mut ShellControl<'_>) {
        // Storage job pump, ahead of everything below: a job that settled
        // since the last frame must be applied even on an iteration that
        // paints nothing. Anything that settles off-thread wakes us through
        // the shell's host waker.
        self.state.pump_storage();
        let state = &self.state;
        self.frame_probe.wakeup(|| state.pending_op_count_all());

        self.track_normal_bounds();

        // Deferred close: the user answered "Save" to the close prompt and
        // that save has now landed (the pump above ran its continuation,
        // which set the flag). Re-validate before acting on it — the user
        // may have kept editing while the write was in flight. See
        // `close_gate`.
        match deferred_close_decision(
            self.close_when_idle.load(Ordering::SeqCst),
            self.state.has_unsaved_changes(),
        ) {
            DeferredClose::NotRequested => {}
            DeferredClose::CancelledByNewEdits => {
                self.close_when_idle.store(false, Ordering::SeqCst);
                self.state.notify(
                    atomartist_ui::NoticeLevel::Info,
                    "Close cancelled — there are unsaved changes made \
                     since you chose Save.",
                );
            }
            DeferredClose::Close => {
                control.request_exit();
            }
        }

        // Persist settings when anything changed AND the user isn't
        // mid-drag. `AutoSave` handles the diff + idle guard. Destructured so
        // the closures borrow disjoint fields.
        let AtomHost {
            settings_auto_save,
            settings_path,
            state,
            debug,
            normal_bounds,
            window,
            ..
        } = &mut *self;
        if let Some(path) = settings_path.as_ref() {
            settings_auto_save.tick(
                control.pointer_idle(),
                || compose_settings_blob(state, debug, normal_bounds, window),
                |blob| write_settings_blob(path, blob),
            );
        }

        // Continuous run-mode (Performance window) keeps the loop spinning
        // every frame so the FPS readout reflects a real sustained
        // framerate, not just per-input wakeups.
        control.set_redraw_policy(
            if self.debug.run_mode.get() == agg_gui::RunMode::Continuous {
                agg_gui_shell::RedrawPolicy::Continuous
            } else {
                agg_gui_shell::RedrawPolicy::Reactive
            },
        );
    }

    fn on_gpu_rebuilt(&mut self, _app: &mut App, gpu: &Gpu) {
        // The old device's error handler died with it.
        install_uncaptured_error_handler(gpu.device());
    }

    fn on_exit(&mut self, _app: &mut App) {
        // `--screenshot` is a headless capture run that never touches a
        // user's document: no settings write, no drain (waiting would only
        // slow the harness).
        if self.screenshot_mode {
            return;
        }
        // Flush settings so the last-opened project path (and theme / window
        // bounds the user just changed) survives even when the close happens
        // between paints, then give in-flight storage work a bounded last
        // chance — the loop is about to stop pumping, so a save whose job
        // has not settled yet would be lost without a word.
        self.flush_settings();
        self.state
            .drain_pending_ops(std::time::Duration::from_secs(5));
    }
}
