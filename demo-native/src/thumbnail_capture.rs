//! Opportunistic viewport-preview capture for the native shell.
//!
//! `.atmr` can carry a `Metadata/thumbnail.png` preview (see
//! `atomartist_lib::serialization::thumbnail`), and saving embeds
//! whatever preview is currently parked in
//! [`AppState::latest_thumbnail`]. Filling that slot is the shell's
//! job, because reading pixels back off the GPU is only possible from
//! inside the frame loop — a save can arrive on any frame and must not
//! stall waiting for a readback.
//!
//! ## Why it is cheap
//!
//! Three separate costs, each kept off the hot path:
//!
//! * **Snapshot** (`capture_screenshot`) is a GPU-side
//!   `copy_texture_to_texture` — no CPU readback, no stall. It runs at
//!   most once per [`CAPTURE_INTERVAL`].
//! * **Readback** uses demo-wgpu's *non-blocking, scaled* pair
//!   (`begin_capture_readback_scaled` / `poll_capture_readback_scaled`),
//!   never the blocking `read_captured_screenshot` the `--screenshot`
//!   path uses. The GPU crops to the preview's source region and
//!   resamples to 256×192 during the blit, so only ~192 KB — not a whole
//!   framebuffer — is mapped back; the map resolves a frame or two later
//!   and the poll is a `try_recv`. The scaled readback has its own
//!   in-flight slot, so it can never collide with a `--screenshot` run's
//!   full-surface readback.
//! * **Encode** (drop alpha → PNG) happens on a spawned thread, so the
//!   only main-thread work is the row-unpadding memcpy inside
//!   `poll_capture_readback_scaled`.
//!
//! ## What ends up in the picture
//!
//! The snapshot is the whole window, but the preview is cropped to the
//! **3-D viewport widget's** rectangle (looked up by id, converted from
//! agg-gui's bottom-up coordinates by
//! [`atomartist_ui::framebuffer_crop_from_widget_rect`]) — a preview of
//! the node canvas and the side panels would tell the user nothing about
//! which model a file holds. The preview's 4:3 aspect crop is applied
//! *within* that rectangle by
//! [`atomartist_ui::thumbnail_source_region`] before the blit, so the
//! rectangle handed to the GPU is exactly what the PNG shows. When the
//! viewport isn't on screen the capture is skipped entirely rather than
//! falling back to the window.
//!
//! Set `ATOMARTIST_THUMB_LOG=1` to print the measured cost of each
//! stage — per CLAUDE.md, the per-frame budget claim above is something
//! to measure, not assume.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use agg_gui::{App, DrawCtx};
use atomartist_ui::{AppState, CropRect, THUMBNAIL_HEIGHT, THUMBNAIL_WIDTH};
use demo_wgpu::{RectInPixels, WgpuGfxCtx};
use web_time::Instant;

/// Widget id of the 3-D viewport in the app tree (see
/// `atomartist_ui::build_app`); the preview is a crop of this widget.
const VIEWPORT_WIDGET_ID: &str = "viewport-3d";

/// Minimum wall-clock gap between two preview snapshots. Long enough
/// that the cost is irrelevant even if a capture were expensive; short
/// enough that a save picks up a preview of roughly the current model.
const CAPTURE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Delay before the very first capture, so startup frames (empty scene,
/// fonts still warming) don't become the preview.
const FIRST_CAPTURE_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

pub struct ThumbnailCapture {
    /// Finished previews travel back from the encode thread over this
    /// channel rather than being written into the state directly:
    /// `AppState` is `!Send` (its undo buffer holds non-`Send` trait
    /// objects), and routing the result through the main thread keeps
    /// [`AppState::set_thumbnail_png`] the single way the slot is ever
    /// filled.
    encoded_tx: Sender<Vec<u8>>,
    encoded_rx: Receiver<Vec<u8>>,
    /// False for `--screenshot` runs: they drive the same capture
    /// texture for a different purpose, and a headless one-shot has no
    /// use for a preview anyway.
    enabled: bool,
    started: Instant,
    last_capture: Option<Instant>,
    /// A snapshot was taken this frame; the readback starts after
    /// present so the copy has been submitted.
    snapshot_taken: bool,
    /// Preview source rectangle (the 4:3 window inside the 3-D
    /// viewport) as it stood when the snapshot was taken — handed to the
    /// GPU blit when the readback starts after present.
    snapshot_region: Option<RectInPixels>,
    /// Set while an encode thread is in flight, so a slow encode can't
    /// pile threads up behind a fast capture cadence.
    encoding: Arc<AtomicBool>,
}

impl ThumbnailCapture {
    pub fn new(enabled: bool) -> Self {
        let (encoded_tx, encoded_rx) = std::sync::mpsc::channel();
        Self {
            encoded_tx,
            encoded_rx,
            enabled,
            started: Instant::now(),
            last_capture: None,
            snapshot_taken: false,
            snapshot_region: None,
            encoding: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Called from `paint_frame` between `end_frame` and `present` —
    /// the only window where the surface texture holds this frame's
    /// finished image and still exists.
    ///
    /// `surface_w` / `surface_h` are the framebuffer's dimensions, which
    /// on this shell are also the coordinate space the widget tree was
    /// laid out in.
    pub fn before_present(
        &mut self,
        ctx: &mut WgpuGfxCtx,
        app: &App,
        surface_w: u32,
        surface_h: u32,
    ) {
        if !self.enabled || !self.due() || ctx.has_pending_scaled_readback() {
            return;
        }
        // No viewport on screen (hidden, collapsed, or not laid out
        // yet) means there is nothing worth previewing — skip rather
        // than capture a window full of panels.
        let Some(viewport) = viewport_region(app, surface_w, surface_h) else {
            return;
        };
        let region = source_rect_in_pixels(viewport);
        let t = Instant::now();
        if ctx.capture_screenshot() {
            self.snapshot_taken = true;
            self.snapshot_region = Some(region);
            self.last_capture = Some(Instant::now());
            log_stage("snapshot", t);
        }
    }

    /// Called from `paint_frame` after `present`: starts the readback
    /// for a snapshot taken this frame, harvests any readback that has
    /// completed, and publishes any preview an encode thread finished.
    /// All three are non-blocking.
    pub fn after_present(&mut self, ctx: &mut WgpuGfxCtx, state: &AppState) {
        if !self.enabled {
            return;
        }
        // Publish whatever the encode thread finished since last frame.
        while let Ok(png) = self.encoded_rx.try_recv() {
            state.set_thumbnail_png(png);
        }
        if self.snapshot_taken {
            self.snapshot_taken = false;
            let t = Instant::now();
            // The crop *and* the downscale happen on the GPU here: only
            // THUMBNAIL_WIDTH x THUMBNAIL_HEIGHT RGBA pixels are ever
            // mapped back.
            let region = self.snapshot_region.take();
            ctx.begin_capture_readback_scaled(region, THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT);
            log_stage("readback_begin", t);
            return; // the map cannot possibly be ready in the same frame
        }
        if !ctx.has_pending_scaled_readback() {
            return;
        }
        let t = Instant::now();
        let Some((pixels, w, h)) = ctx.poll_capture_readback_scaled() else {
            return;
        };
        log_stage("readback_harvest", t);
        if pixels.is_empty() || w == 0 || h == 0 {
            return;
        }
        if self.encoding.swap(true, Ordering::SeqCst) {
            return; // an earlier encode is still running
        }
        let tx = self.encoded_tx.clone();
        let flag = self.encoding.clone();
        std::thread::spawn(move || {
            let t = Instant::now();
            // Already cropped and resampled by the blit — all that is
            // left is shedding alpha and running the PNG encoder.
            if let Some(png) = atomartist_ui::thumbnail_png_from_exact_rgba(&pixels, w, h) {
                // A closed receiver just means the app is shutting
                // down; the preview is disposable either way.
                let _ = tx.send(png);
            }
            log_stage("encode(thread)", t);
            flag.store(false, Ordering::SeqCst);
        });
    }

    fn due(&self) -> bool {
        match self.last_capture {
            Some(prev) => prev.elapsed() >= CAPTURE_INTERVAL,
            None => self.started.elapsed() >= FIRST_CAPTURE_DELAY,
        }
    }
}

/// Framebuffer rectangle covering the 3-D viewport widget, or `None`
/// when it isn't in the tree / isn't visibly on the surface.
///
/// `Widget::bounds` is in agg-gui's bottom-up window coordinates; the
/// flip and the clip both live in `atomartist_ui::thumbnail` so they can
/// be unit-tested without a window.
fn viewport_region(app: &App, surface_w: u32, surface_h: u32) -> Option<CropRect> {
    let widget = agg_gui::find_widget_by_id(app.root(), VIEWPORT_WIDGET_ID)?;
    let b = widget.bounds();
    atomartist_ui::framebuffer_crop_from_widget_rect(
        b.x, b.y, b.width, b.height, surface_w, surface_h,
    )
}

/// The blit's source rectangle: the preview's 4:3 window inside the
/// viewport, expressed in demo-wgpu's top-down framebuffer pixels.
///
/// Both `CropRect` and `RectInPixels` are already top-down (the flip
/// happened in `framebuffer_crop_from_widget_rect`), so this is a pure
/// field rename around the shared aspect math.
fn source_rect_in_pixels(viewport: CropRect) -> RectInPixels {
    let r = atomartist_ui::thumbnail_source_region(viewport);
    RectInPixels::new(r.x, r.y, r.w, r.h)
}

fn thumb_log_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("ATOMARTIST_THUMB_LOG")
            .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "on" | "yes"))
            .unwrap_or(false)
    })
}

fn log_stage(stage: &str, t: Instant) {
    if thumb_log_enabled() {
        eprintln!(
            "[thumbnail] {stage} {:.3} ms",
            t.elapsed().as_secs_f32() * 1000.0
        );
    }
}
