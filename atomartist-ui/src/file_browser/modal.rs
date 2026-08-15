//! The Open/Save modal host (design §4 `modal.rs` row, step 6c-1).
//!
//! Three pieces:
//!
//! - [`FileBrowserModalHandle`] — the channel anything *outside* the
//!   widget tree uses to put the dialog up.
//!   [`open`](FileBrowserModalHandle::open) returns a
//!   [`Job<Option<StorageUri>>`] that settles `Some(uri)` on confirm and
//!   `None` on Cancel / Escape. That is deliberately the shape step 6c-2's
//!   `FileDialogProvider` will hand back to `menu_actions`.
//! - [`FileBrowserModalHost`] — the always-present widget that lives at
//!   the top of the app's root `Stack`. It holds no dialog while closed;
//!   an open request makes it build one, and the close settles the job.
//! - [`super::modal_panel::FileBrowserModal`] — the panel chrome inside
//!   the sheet.
//!
//! # Why `agg_gui::widgets::ModalSheet` and not a local overlay
//!
//! The sheet already provides exactly the three things a picker needs and
//! that [`crate::floating_overlay::FloatingOverlayHost`] (the house
//! alternative, built for a *draggable, non-modal* colour picker) does
//! not: a dimming scrim, `Widget::has_active_modal` so agg-gui routes all
//! pointer and key input into the sheet subtree regardless of what is
//! underneath, and Escape-closes. Its one constraint — content is handed
//! in at construction — is not a problem here, because the host builds a
//! *fresh* sheet per open anyway (see below). No agg-gui change was
//! needed.
//!
//! # Fresh model per open, shared cache across opens
//!
//! Each open builds a new [`BrowserModel`] with
//! [`BrowserModel::opened_on`], which starts a listing immediately — so
//! the dialog always shows the directory as it is *now*, never a listing
//! cached from a previous open. The [`ThumbnailCache`], by contrast, is
//! owned by the host and outlives every dialog: previews are expensive,
//! immutable for a given `(uri, stamp)`, and re-reading them on each open
//! would be the one obviously wasteful thing a picker can do.
//!
//! # Exactly-once settlement
//!
//! The [`JobCompleter`] lives in the host's session and is `take`n when
//! the session closes, so the job settles once and only once no matter how
//! many times OK is clicked or whether Escape follows a confirm. The
//! buttons never settle anything themselves: OK records a pick and lowers
//! the visibility cell, and the host — which observes that cell in
//! `layout`, `paint`, and `on_event` — does the settling.
//!
//! # Not in this step
//!
//! Overwrite confirmation. Save mode returns the joined URI even when it
//! names an existing entry; `menu_actions`' save path already owns the
//! precondition handling, and the confirm dialog is a 6c-2/6d concern.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use agg_gui::text::Font;
use agg_gui::widgets::ModalSheet;
use agg_gui::{
    DrawCtx, Event, EventResult, HAnchor, Insets, Point, Rect, Size, VAnchor, Widget, WidgetBase,
};
use atomartist_storage::{Job, JobCompleter, StorageError, StorageUri};

use super::modal_panel::FileBrowserModal;
use super::model::BrowserModel;
use super::thumbs::ThumbnailCache;
use super::widget::{BrowserMode, FileBrowser};
use crate::app_state::AppState;

/// Panel size of the dialog. `ModalSheet` clamps it to the window.
pub const PANEL_SIZE: Size = Size {
    width: 760.0,
    height: 520.0,
};

/// Extension a save-mode name gets when the user typed none.
pub const PROJECT_EXTENSION: &str = "atmr";

/// One queued "put the picker up" request.
struct OpenRequest {
    mode: BrowserMode,
    default_name: String,
    /// Extension a save-mode name is forced to — see
    /// [`FileBrowserModalHandle::open_with_extension`].
    extension: String,
    completer: JobCompleter<Option<StorageUri>>,
}

#[derive(Default)]
struct HandleInner {
    pending: Option<OpenRequest>,
    open: bool,
}

/// Cross-boundary handle to the Open/Save dialog.
///
/// `Send + Sync` on purpose: step 6c-2's `FileDialogProvider`
/// implementation holds one, and that trait is `Send + Sync`. Nothing in
/// here touches a widget — the host drains the request on its next
/// layout, on the UI thread.
#[derive(Clone)]
pub struct FileBrowserModalHandle {
    inner: Arc<Mutex<HandleInner>>,
}

impl FileBrowserModalHandle {
    pub fn new() -> Self {
        FileBrowserModalHandle {
            inner: Arc::new(Mutex::new(HandleInner::default())),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HandleInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Put the dialog up and hand back the job its outcome settles into.
    ///
    /// `default_name` seeds the save-mode name field and is ignored in
    /// open mode.
    ///
    /// A second call while a dialog is already up (or one is queued) is
    /// refused: the live dialog is never stolen, and the *new* job fails
    /// with [`StorageError::Cancelled`]. That is deliberately not
    /// `Ok(None)` — a menu that double-fires must not read as "the user
    /// cancelled" and, say, silently abandon a save the user is in the
    /// middle of. Callers that want to avoid the refusal entirely check
    /// [`is_open`](Self::is_open) first.
    pub fn open(&self, mode: BrowserMode, default_name: &str) -> Job<Option<StorageUri>> {
        self.open_with_extension(mode, default_name, PROJECT_EXTENSION)
    }

    /// [`open`](Self::open) for a destination that is *not* a project.
    ///
    /// `extension` (lowercase, no dot) is the extension a save-mode name
    /// is forced to by [`resolve_pick`] — `"stl"` for File → Export → STL,
    /// and so on. Without it every export would come back named `.atmr`,
    /// because forcing the project extension is exactly what the plain
    /// [`open`](Self::open) does.
    ///
    /// In [`BrowserMode::Open`] the extension is carried but unused: the
    /// pick is an entry that already exists. Filtering the *listing* by
    /// extension is deliberately not part of this step (design §5, 6d
    /// polish) — an import picker still shows everything.
    pub fn open_with_extension(
        &self,
        mode: BrowserMode,
        default_name: &str,
        extension: &str,
    ) -> Job<Option<StorageUri>> {
        let (job, completer) = Job::pending();
        let mut inner = self.lock();
        if inner.open || inner.pending.is_some() {
            drop(inner);
            completer.fail(StorageError::Cancelled);
            return job;
        }
        inner.pending = Some(OpenRequest {
            mode,
            default_name: default_name.to_string(),
            extension: extension.to_string(),
            completer,
        });
        drop(inner);
        agg_gui::animation::request_draw();
        job
    }

    /// `true` while the dialog is on screen (or its request is queued) —
    /// for tests, and for callers that want to avoid stacking pickers.
    pub fn is_open(&self) -> bool {
        let inner = self.lock();
        inner.open || inner.pending.is_some()
    }

    fn take_request(&self) -> Option<OpenRequest> {
        self.lock().pending.take()
    }

    fn set_open(&self, open: bool) {
        self.lock().open = open;
    }
}

impl Default for FileBrowserModalHandle {
    fn default() -> Self {
        FileBrowserModalHandle::new()
    }
}

/// What one open dialog carries while it is up.
struct Session {
    /// Shared with the [`ModalSheet`]; lowered by OK, Cancel, or Escape.
    visible: Rc<Cell<bool>>,
    /// Filled by OK. `None` at close time means "cancelled".
    outcome: Rc<RefCell<Option<StorageUri>>>,
    /// Taken exactly once, when the session closes.
    completer: Option<JobCompleter<Option<StorageUri>>>,
}

/// A session that goes away with its job still pending — the window
/// closing, the whole tree being torn down — cancels that job.
///
/// Without this the completer's own `Drop` fails the job with an
/// `Io("storage worker dropped…")` error, which is both untrue (there is
/// no worker) and unhandled by callers that were promised `Some` / `None`.
/// The normal close path has already `take`n the completer by the time the
/// session drops, so this only ever fires on the abnormal one.
impl Drop for Session {
    fn drop(&mut self) {
        if let Some(completer) = self.completer.take() {
            completer.succeed(None);
        }
    }
}

/// Screen-filling, input-transparent host for the Open/Save dialog.
///
/// Place it last in the app's root `Stack`. While closed it holds no
/// children at all, so it costs one no-op layout call per frame and
/// hit-tests as absent; while open its single child is a [`ModalSheet`],
/// which agg-gui's `active_modal_path` routes every event to.
pub struct FileBrowserModalHost {
    handle: FileBrowserModalHandle,
    state: AppState,
    font: Arc<Font>,
    /// Shared across every dialog this host opens — see the module docs.
    cache: ThumbnailCache,
    /// Empty, or exactly one `ModalSheet`.
    children: Vec<Box<dyn Widget>>,
    session: Option<Session>,
    bounds: Rect,
    base: WidgetBase,
}

impl FileBrowserModalHost {
    pub fn new(state: AppState, font: Arc<Font>, handle: FileBrowserModalHandle) -> Self {
        FileBrowserModalHost {
            handle,
            state,
            font,
            cache: ThumbnailCache::new(),
            children: Vec::new(),
            session: None,
            bounds: Rect::default(),
            base: WidgetBase::new()
                .with_h_anchor(HAnchor::STRETCH)
                .with_v_anchor(VAnchor::STRETCH),
        }
    }

    pub fn handle(&self) -> FileBrowserModalHandle {
        self.handle.clone()
    }

    /// Settle a dialog whose visibility cell has been lowered (by OK,
    /// Cancel, or Escape) and drop it.
    ///
    /// Called from `layout`, `paint`, and `on_event` so the host stays
    /// consistent whichever pass sees the close first — a click on OK is
    /// consumed by the button and never bubbles up here, so `layout` is
    /// usually the pass that notices.
    ///
    /// # A job that settles behind the dialog's back
    ///
    /// The pick job can be cancelled by someone who never saw the dialog:
    /// the status bar's "cancel all storage activity", or the shutdown
    /// drain, both reach the [`crate::storage_ops::JobOp`] wrapping it and
    /// settle it [`StorageError::Cancelled`]. Leaving the sheet up then
    /// strands the user in front of a picker whose OK cannot do anything —
    /// the completer would silently ignore it. So a session whose
    /// completer reports [`JobCompleter::is_settled`] lowers its own
    /// visibility and takes the ordinary close path from there. That path
    /// still calls `succeed`, which an already-settled job ignores, so
    /// nothing is settled twice and the cancellation stands.
    fn settle_if_closed(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if session
            .completer
            .as_ref()
            .is_some_and(JobCompleter::is_settled)
        {
            session.visible.set(false);
        }
        if session.visible.get() {
            return;
        }
        let picked = session.outcome.borrow_mut().take();
        if let Some(completer) = session.completer.take() {
            completer.succeed(picked);
        }
        self.session = None;
        self.children.clear();
        self.handle.set_open(false);
        agg_gui::animation::request_draw();
    }

    /// Claim a queued open request. **Only `layout` calls this**: a sheet
    /// adopted during `paint` would be painted in the same pass having
    /// never been laid out, i.e. a zero-size scrim for one frame. The
    /// request already asked for a draw, so the next frame's layout picks
    /// it up with no visible delay.
    fn claim_pending(&mut self) {
        if self.session.is_some() {
            return;
        }
        if let Some(request) = self.handle.take_request() {
            self.open_dialog(request);
        }
    }

    /// Build a fresh sheet for `request` and adopt it as our only child.
    fn open_dialog(&mut self, request: OpenRequest) {
        let visible = Rc::new(Cell::new(true));
        let outcome: Rc<RefCell<Option<StorageUri>>> = Rc::new(RefCell::new(None));
        let mode = request.mode;

        // Fresh model = fresh listing on every open.
        let model = BrowserModel::opened_on(&self.state);

        // Double-clicking a file in open mode *is* pressing Open. In save
        // mode the widget has already copied the name into the name
        // field, which is what the ancestors do, so activation there is
        // not a confirm.
        let activate_outcome = Rc::clone(&outcome);
        let activate_visible = Rc::clone(&visible);
        let browser = FileBrowser::new(self.state.clone(), model.clone(), self.cache.clone(), mode)
            .on_activate(move |entry| {
                if mode == BrowserMode::Open && !entry.is_dir {
                    *activate_outcome.borrow_mut() = Some(entry.uri.clone());
                    activate_visible.set(false);
                    agg_gui::animation::request_draw();
                }
            });
        if mode.shows_name_field() {
            browser.set_name_text(request.default_name.clone());
        }
        let name_cell = browser.name_cell();

        let gate_model = model.clone();
        let gate_name = Rc::clone(&name_cell);
        let gate_ext = request.extension.clone();
        let ok_enabled: Rc<dyn Fn() -> bool> = Rc::new(move || {
            resolve_pick(mode, &gate_model, &gate_name.borrow(), &gate_ext).is_some()
        });

        let ok_model = model;
        let ok_ext = request.extension.clone();
        let ok_name = Rc::clone(&name_cell);
        let ok_outcome = Rc::clone(&outcome);
        let ok_visible = Rc::clone(&visible);
        let on_ok = move || {
            // Authoritative guard: the button's `enabled_fn` already hides
            // this case, but a refused pick must never close the dialog.
            let picked = {
                let name = ok_name.borrow();
                resolve_pick(mode, &ok_model, &name, &ok_ext)
            };
            if let Some(uri) = picked {
                *ok_outcome.borrow_mut() = Some(uri);
                ok_visible.set(false);
                agg_gui::animation::request_draw();
            }
        };

        let cancel_visible = Rc::clone(&visible);
        let on_cancel = move || {
            cancel_visible.set(false);
            agg_gui::animation::request_draw();
        };

        let panel = FileBrowserModal::new(
            mode,
            self.font.clone(),
            browser,
            ok_enabled,
            on_ok,
            on_cancel,
        );
        let sheet =
            ModalSheet::new(Rc::clone(&visible), Box::new(panel)).with_panel_size(PANEL_SIZE);

        self.children.push(Box::new(sheet));
        self.session = Some(Session {
            visible,
            outcome,
            completer: Some(request.completer),
        });
        self.handle.set_open(true);
        agg_gui::animation::request_draw();
    }
}

/// What OK would produce right now, or `None` when OK is meaningless.
///
/// Open mode picks the selected *file* — resolved through
/// [`BrowserModel::selected_entry`], not the raw `selected()` URI, so a
/// selection the listing no longer contains cannot be opened.
///
/// Save mode names a file **in the directory on screen**: navigation is
/// what the browser is for, so a name carrying a separator (`/` or `\`)
/// is refused rather than quietly interpreted as a path. So is an empty
/// name, and so is anything [`StorageUri::try_join`] rejects — it is the
/// traversal guard, and this is a name the *user* authored. The extension
/// is forced to `extension` by [`with_forced_extension`] — `"atmr"` for
/// the project picker, the format's own for File → Export (see
/// [`FileBrowserModalHandle::open_with_extension`]).
pub fn resolve_pick(
    mode: BrowserMode,
    model: &BrowserModel,
    name: &str,
    extension: &str,
) -> Option<StorageUri> {
    match mode {
        BrowserMode::Open => model
            .selected_entry()
            .filter(|entry| !entry.is_dir)
            .map(|entry| entry.uri),
        BrowserMode::Save => {
            let cwd = model.cwd()?;
            let typed = name.trim();
            if typed.is_empty() || typed.contains('/') || typed.contains('\\') {
                return None;
            }
            let joined = cwd.try_join(&with_forced_extension(typed, extension)?).ok()?;
            // Belt and braces: nothing that survives the rules above can
            // normalise back to the directory itself, and saving *over* a
            // directory is not a thing we ever want to hand a caller.
            (joined != cwd).then_some(joined)
        }
    }
}

/// Force `extension` onto a user-typed name.
///
/// A save-mode picker has exactly one format — the project picker saves
/// `.atmr`, the STL export picker saves `.stl` — so, like `rfd`'s
/// single-filter save dialog on native, the answer always ends in it.
/// With `extension = "atmr"`:
///
/// - `Version 1.2` → `Version 1.2.atmr`. A dot mid-name is part of the
///   name, not an extension the user chose.
/// - `design.` → `design.atmr`. Trailing dots are trimmed first; Windows
///   refuses to create a file whose name ends in one.
/// - `.atmr` → `.atmr.atmr`. A leading dot starts the *name*, so that
///   input has no extension at all — the dotfile rule `split_stem_ext`
///   (`crate::app_state_storage`) documents and `uri_extension` applies.
/// - `bracket.atmr` / `BRACKET.ATMR` → unchanged, matched case-insensitively.
///
/// `None` when nothing is left after trimming (`.`, `...`).
fn with_forced_extension(name: &str, extension: &str) -> Option<String> {
    let trimmed = name.trim_end_matches('.');
    if trimmed.is_empty() {
        return None;
    }
    let suffix = format!(".{extension}");
    let already =
        trimmed.len() > suffix.len() && trimmed.to_ascii_lowercase().ends_with(suffix.as_str());
    Some(if already {
        trimmed.to_string()
    } else {
        format!("{trimmed}{suffix}")
    })
}

impl Widget for FileBrowserModalHost {
    fn type_name(&self) -> &'static str {
        "FileBrowserModalHost"
    }
    fn bounds(&self) -> Rect {
        self.bounds
    }
    fn set_bounds(&mut self, b: Rect) {
        self.bounds = b;
    }
    fn children(&self) -> &[Box<dyn Widget>] {
        &self.children
    }
    fn children_mut(&mut self) -> &mut Vec<Box<dyn Widget>> {
        &mut self.children
    }
    fn margin(&self) -> Insets {
        Insets::ZERO
    }
    fn h_anchor(&self) -> HAnchor {
        self.base.h_anchor
    }
    fn v_anchor(&self) -> VAnchor {
        self.base.v_anchor
    }
    fn widget_base(&self) -> Option<&WidgetBase> {
        Some(&self.base)
    }
    fn widget_base_mut(&mut self) -> Option<&mut WidgetBase> {
        Some(&mut self.base)
    }

    fn layout(&mut self, available: Size) -> Size {
        self.settle_if_closed();
        self.claim_pending();
        self.bounds = Rect::new(0.0, 0.0, available.width, available.height);
        if let Some(child) = self.children.first_mut() {
            child.layout(available);
            child.set_bounds(Rect::new(0.0, 0.0, available.width, available.height));
        }
        available
    }

    fn paint(&mut self, _ctx: &mut dyn DrawCtx) {
        // Nothing of our own; `paint_subtree` recurses into the sheet,
        // which draws the scrim and the panel chrome. Closing here (but
        // never *opening* — see `claim_pending`) keeps a dialog dismissed
        // during a paint-only frame from drawing one frame too long.
        self.settle_if_closed();
    }

    /// Never claim a hit. While a dialog is up, agg-gui reaches it through
    /// `active_modal_path` (which ignores hit-testing entirely) and the
    /// sheet's scrim swallows whatever the panel does not use, so nothing
    /// behind the dialog can react. While closed, returning `false` lets
    /// every event fall through to the rest of the `Stack`.
    fn hit_test(&self, _local_pos: Point) -> bool {
        false
    }

    fn on_event(&mut self, _event: &Event) -> EventResult {
        self.settle_if_closed();
        EventResult::Ignored
    }

    fn properties(&self) -> Vec<(&'static str, String)> {
        vec![("open", self.session.is_some().to_string())]
    }
}

#[cfg(test)]
#[path = "modal_tests.rs"]
mod modal_tests;
