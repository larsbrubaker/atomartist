//! The frame-loop job pump — how an asynchronous [`StorageProvider`] gets
//! its result back into the UI.
//!
//! `atomartist-storage` hands every slow call back as a
//! [`Job<T>`](atomartist_storage::Job): a slot the caller polls rather than
//! awaits. Something has to do the polling, and per the architecture plan
//! (`docs/storage-architecture-plan.md` §3.3) that something is the frame
//! loop. This module supplies the two halves of that arrangement:
//!
//! - [`PendingOp`] — an object-safe "job plus what to do when it lands".
//!   [`JobOp`] is the one implementation we need so far: a single job and a
//!   continuation closure.
//! - The [`AppState`] pump — [`AppState::submit_op`] hands an operation in,
//!   [`AppState::pump_storage`] is called once per frame by each shell
//!   (`demo-native`'s event loop, `demo-wasm`'s `render`, and
//!   `TestHarness::pump`) and applies everything that has settled.
//!
//! # Loud and quiet operations
//!
//! Everything the user asked for by name — open, save, export — is *loud*:
//! it appears in the status bar's activity readout, it makes
//! [`AppState::pending_op_count`] non-zero (which is what
//! [`crate::menu_actions`] refuses a second File action on), and the
//! shutdown drain waits for it.
//!
//! Background work the user never asked for — or that reports itself
//! somewhere better — is *quiet* ([`JobOp::new_quiet`]). Two kinds so far,
//! both from the file browser:
//!
//! - **Thumbnail reads** ([`crate::file_browser::thumbs`]): a directory of
//!   previews would otherwise chatter `Preview a.atmr… (+11 more)` across
//!   the status bar, make every File menu action report "storage is busy",
//!   and hold the window open at exit for images nobody will see.
//! - **Directory listings** ([`crate::file_browser::model`]): the browser
//!   paints its own `Loading` / `Empty` / `Error` state in the pane the
//!   user is looking at (design §2, "never a blank pane"), so the status
//!   bar has nothing to add. Louder than that is actively wrong: a
//!   listing in flight would make [`crate::menu_actions`] refuse File →
//!   Open *and* the favorites bar's own project opens, and on an
//!   asynchronous provider something is listing for most of the time the
//!   browser is on screen. A failed listing is still reported — in the
//!   pane, by the browser.
//!
//! A quiet
//! operation is therefore excluded from [`AppState::pending_op_status`]
//! (and so from [`AppState::storage_activity_text`]) and from
//! [`AppState::pending_op_count`], and [`AppState::drain_pending_ops`]
//! cancels rather than waits for it. It is otherwise an ordinary
//! operation: pumped on the same queue, applied by the same code, and
//! cancelled by [`AppState::cancel_pending_ops`] along with everything
//! else — a user who cancels "all storage activity" is not surprised to
//! find previews cancelled too.
//!
//! A continuation runs outside the widget tree, so it has no dialog
//! provider to talk to. Instead it leaves a [`Notice`] on the state
//! ([`AppState::notify`]); [`AppState::pump_notices`] — called from
//! [`AppState::pump_storage`], i.e. once per frame on every shell —
//! drains the queue, prints errors to stderr, and parks the newest
//! message in [`AppState::last_notice`] for the status bar to paint.
//!
//! Synchronous providers (`LocalFsProvider`, `MemoryProvider`) settle their
//! jobs before `submit_op` returns, so `submit_op` applies those inline and
//! the desktop path never waits a frame — the pump exists for the providers
//! that cannot do that.
//!
//! # Wakeup strategy (resolves 4a's deferred item 2)
//!
//! Both wakeups go through `agg_gui::animation::signal_async_state_change`,
//! which bumps a **process-global atomic** that the main thread merges into
//! its thread-local draw flags on its next `wants_draw` /
//! `invalidation_epoch` / `async_state_epoch` read. A plain `request_draw`
//! would only set a thread-local the signalling thread itself never reads.
//!
//! What that does and does not buy, precisely:
//!
//! - [`AppState::notify`] only touches the notice mutex and that signal,
//!   so it is safe from any thread.
//! - [`AppState::submit_op`] is safe from any thread **only for a job that
//!   is genuinely still pending** — that path just parks the operation on
//!   the queue. When the job may already be settled, which is every
//!   operation on every local provider, the continuation runs *inline on
//!   the calling thread*, mutating [`AppState`] and touching agg-gui
//!   thread-locals, so it must be called on the main thread.
//! - In practice both are main-thread calls today regardless: [`AppState`]
//!   is `!Send` (agg-gui's `UndoBuffer` holds `Box<dyn UndoRedoCommand>`
//!   with no `Send` bound), so a worker cannot hold one. The global signal
//!   is still the right primitive — it is what a future `Send` handle, or
//!   a provider that wakes us from its own thread, will need.
//!
//! That signal also bumps the async-state epoch, which makes `App::paint`
//! mark the whole tree dirty once — correct here, because a submitted or
//! finished operation is exactly the kind of out-of-band change retained
//! backbuffers cannot otherwise see, and it happens per *operation*, not
//! per frame.
//!
//! What brings the loop back for a queued operation is
//! [`crate::storage_wakeup`]'s business: a settling
//! [`atomartist_storage::JobCompleter`] fires the storage completion hook
//! (which every shell points at `signal_async_state_change`, and which the
//! native shell chains into agg-gui's host waker), and the pump keeps a
//! per-frame keep-alive only while a *progress-reporting* operation is on
//! screen — progress being the one thing nobody signals. Through step 6f
//! this branch asked for a frame every frame while anything at all was
//! queued, which pinned the app at full framerate for as long as an idle
//! file dialog was open.
//!
//! # Keep-alive cost (resolves 4a's deferred item 1)
//!
//! What is left of the keep-alive uses `request_draw_without_invalidation`,
//! reserving `request_draw` for the frames that actually apply a
//! continuation. Reasoning, from the agg-gui sources rather than a guess:
//! nothing re-rasters off `invalidation_epoch` — retained backbuffers key
//! on their dirty flag, the theme / typography / async-state epochs, and
//! `Widget::needs_draw` (`widget/paint/offscreen.rs`); `invalidation_epoch`
//! is read only by `dispatch_event` (to dirty ancestors of a widget that
//! changed during event delivery) and, in this app, by `demo-native`'s
//! debug-inspector snapshot cache, where every bump costs a full
//! `collect_inspector_nodes()` walk. Bumping it once per frame for the
//! whole duration of a download therefore buys nothing and costs that
//! walk whenever the inspector is open. The status bar re-rasters through
//! [`crate::status_bar::StatusBar`]'s `needs_draw()` — the trait's
//! purpose-built channel for an ongoing draw need, which propagates into
//! retained ancestors — and it reports that need on exactly the same
//! condition the keep-alive uses, so the two cannot disagree about whether
//! the app is allowed to sleep.
//!
//! Relationship to [`crate::app_state_storage`]: that module hands out
//! the provider [`Job`]s and stops there. Every call site in
//! [`crate::app_state_files`], [`crate::app_state_files_import`], and
//! [`crate::menu_actions`] wraps one in a [`JobOp`] and submits it here —
//! the synchronous `await_job` bridge those call sites used through
//! Phase 4b is gone.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use atomartist_storage::{Job, JobState, StorageError};

use crate::app_state::AppState;

/// How prominently a [`Notice`] should be shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    /// Progress / confirmation text — the status bar is enough.
    Info,
    /// Nothing failed, but the result is not quite what was asked for and
    /// the user has to hear about it. The Boolean node's degraded union
    /// (plan step B-5) is the first source: the node succeeded, every part
    /// is in the output, and some of them could not be combined.
    Warning,
    /// Something failed and the user needs to know.
    Error,
}

impl NoticeLevel {
    /// Severity order, for "the loudest message in a batch wins" and "a
    /// quieter one never displaces a louder one still on screen".
    fn rank(self) -> u8 {
        match self {
            NoticeLevel::Info => 0,
            NoticeLevel::Warning => 1,
            NoticeLevel::Error => 2,
        }
    }
}

/// A user-facing message produced by a storage continuation.
///
/// Continuations run from the frame pump, far away from any widget or
/// dialog provider, so they post messages here instead of trying to show
/// UI themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub level: NoticeLevel,
    pub text: String,
}

/// One storage operation in flight, plus the continuation that applies its
/// result.
///
/// Object-safe on purpose: the pending queue is a `Vec<Box<dyn PendingOp>>`
/// holding operations over completely different payload types.
pub trait PendingOp: Send {
    /// Short user-facing label for the status bar, e.g. `"Opening
    /// bracket.atmr"`.
    fn label(&self) -> &str;

    /// State of the underlying work. The pump applies the operation as
    /// soon as this reports [`JobState::is_settled`].
    fn poll(&self) -> JobState;

    /// Ask the operation to stop. A cancelled job settles as
    /// [`StorageError::Cancelled`], so the next pump still applies the
    /// continuation — with that error.
    fn cancel(&self);

    /// Called once, after the job settles, to apply the result.
    fn apply(self: Box<Self>, state: &AppState);

    /// Background work the user did not ask for by name — see the module
    /// docs. Loud by default: an operation has to opt *out* of being
    /// visible, so a new call site cannot hide a save by forgetting a flag.
    fn is_quiet(&self) -> bool {
        false
    }
}

/// The general-purpose [`PendingOp`]: one [`Job`] and a closure to run when
/// it settles.
///
/// Every storage operation the app performs so far is of this shape (read
/// bytes then load them, write bytes then re-baseline, stat then decide),
/// so this is the only implementation needed. An operation that chains
/// several jobs simply submits the next one from inside its continuation —
/// which the pump supports (see [`AppState::pump_storage`]).
pub struct JobOp<T> {
    label: String,
    job: Job<T>,
    finish: Box<dyn FnOnce(&AppState, Result<T, StorageError>) + Send>,
    quiet: bool,
}

impl<T> JobOp<T> {
    pub fn new(
        label: impl Into<String>,
        job: Job<T>,
        finish: impl FnOnce(&AppState, Result<T, StorageError>) + Send + 'static,
    ) -> Self {
        JobOp {
            label: label.into(),
            job,
            finish: Box::new(finish),
            quiet: false,
        }
    }

    /// A [`JobOp`] the user never asked for: kept out of the status bar and
    /// out of the shutdown wait (module docs, "Loud and quiet
    /// operations"). The label is still carried, for diagnostics.
    pub fn new_quiet(
        label: impl Into<String>,
        job: Job<T>,
        finish: impl FnOnce(&AppState, Result<T, StorageError>) + Send + 'static,
    ) -> Self {
        JobOp {
            quiet: true,
            ..JobOp::new(label, job, finish)
        }
    }
}

impl<T: Send> PendingOp for JobOp<T> {
    fn label(&self) -> &str {
        &self.label
    }

    fn poll(&self) -> JobState {
        self.job.poll()
    }

    fn cancel(&self) {
        self.job.cancel();
    }

    fn is_quiet(&self) -> bool {
        self.quiet
    }

    fn apply(self: Box<Self>, state: &AppState) {
        let this = *self;
        match this.job.take() {
            Some(result) => (this.finish)(state, result),
            // Only reachable if the job was pending (the pump checks
            // first) or was taken by someone else — this handle is not
            // shared. Report it rather than dropping the operation on
            // the floor, because a silently-vanished save is the worst
            // possible failure mode here.
            None => state.notify(
                NoticeLevel::Error,
                format!("{} did not produce a result", this.label),
            ),
        }
    }
}

/// Queue of operations awaiting their job, shared with clones of
/// [`AppState`].
pub type PendingOps = Arc<Mutex<Vec<Box<dyn PendingOp>>>>;

/// Queue of messages awaiting the next paint.
pub type Notices = Arc<Mutex<Vec<Notice>>>;

/// The most recently drained message, held for display.
pub type LastNotice = Arc<Mutex<Option<Notice>>>;

/// Hard cap on the undrained notice queue.
///
/// The UI drains once per frame, so in a running app the queue never holds
/// more than one frame's worth. A headless embedder that never pumps would
/// otherwise grow it without bound; past the cap the oldest messages are
/// dropped, because the newest are the ones worth showing.
pub const NOTICE_CAP: usize = 100;

/// How many cancel-and-pump rounds [`AppState::drain_pending_ops`] runs
/// after its deadline. Enough for a continuation to chain a follow-up (and
/// for that one to chain another); short enough that a runaway chain
/// cannot hold the window open.
#[cfg(not(target_arch = "wasm32"))]
pub const FINAL_PUMP_ROUNDS: usize = 4;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Post a message onto a notice queue, honouring [`NOTICE_CAP`].
///
/// The body of [`AppState::notify`], factored out for the one caller that
/// has a queue but *not* an [`AppState`]:
/// [`crate::file_browser::ModalFileDialogs`] must be `Send + Sync` (the
/// dialog trait is) and `AppState` is `!Send`, so it holds the queue
/// directly. Any-thread safe for the same reason `notify` is.
///
/// # Consecutive duplicates are dropped
///
/// A message identical (same level, same text) to the one already at the
/// tail of the queue is not enqueued. One failure now reaches the queue
/// twice by two legitimate routes — the continuation's own
/// [`AppState::notify`] and a [`crate::top_menu_bar::FileDialogProvider`]
/// whose `show_error` posts here too, which is what
/// `ModalFileDialogs` does in the absence of a message dialog — and the
/// user should not read the same sentence twice. Only *consecutive*
/// duplicates are suppressed: a message that recurs after something else
/// was said is news (the same export failing again, say), and the queue is
/// drained every frame anyway, so this cannot swallow a repeat the user
/// would otherwise have seen as new.
pub(crate) fn push_notice(notices: &Notices, level: NoticeLevel, text: impl Into<String>) {
    {
        let mut queue = lock(notices);
        let text = text.into();
        if queue
            .last()
            .is_some_and(|last| last.level == level && last.text == text)
        {
            return;
        }
        if queue.len() >= NOTICE_CAP {
            let overflow = queue.len() + 1 - NOTICE_CAP;
            queue.drain(0..overflow);
        }
        queue.push(Notice { level, text });
    }
    agg_gui::animation::signal_async_state_change();
}

impl AppState {
    /// Post a message for the UI to show on its next paint.
    ///
    /// Any-thread safe (it only takes the notice mutex and signals the
    /// process-global wakeup) — see the module docs on the wakeup
    /// strategy. Drops the oldest messages once the queue exceeds
    /// [`NOTICE_CAP`], so an embedder that never drains cannot leak.
    pub fn notify(&self, level: NoticeLevel, text: impl Into<String>) {
        push_notice(&self.notices, level, text);
    }

    /// The shared notice queue — for the one collaborator that cannot
    /// hold an [`AppState`]; see [`push_notice`].
    pub(crate) fn notice_queue(&self) -> Notices {
        self.notices.clone()
    }

    /// Take every queued message, leaving the queue empty.
    pub fn drain_notices(&self) -> Vec<Notice> {
        std::mem::take(&mut *lock(&self.notices))
    }

    /// Drain the notice queue into the UI, once per frame.
    ///
    /// Called from the top of [`Self::pump_storage`] — before its
    /// empty-queue early return — so a message posted by an *inline*
    /// continuation (which never touches the pending queue) still reaches
    /// the screen. Errors additionally go to stderr, because a failed save
    /// deserves a record that outlives the status bar.
    ///
    /// # Which message wins the single display slot
    ///
    /// Severity outranks recency, because the worst failure mode this seam
    /// has is a failed save vanishing behind the next operation's
    /// confirmation:
    ///
    /// - Within one drained batch the highest-severity message wins, and
    ///   the newest among equals.
    /// - A quieter notice never displaces a louder undismissed one already
    ///   in the slot — an error stays until another error replaces it or
    ///   the user dismisses it ([`Self::dismiss_notice`], which clears the
    ///   slot for anything).
    ///
    /// Deliberately still the simplest thing that works; the toast /
    /// dialog treatment (with a real message history) arrives with the
    /// file-browser phase.
    ///
    /// Returns how many messages were drained. A notice posted *by* a
    /// continuation therefore surfaces on the following frame — which the
    /// pump has already requested, since applying a continuation always
    /// asks for a draw.
    ///
    /// Errors also go to stderr. On wasm that print is a no-op: the crate
    /// has no `web_sys` dependency, and giving it one just for this is
    /// plumbing that belongs with the Phase 5 browser provider, which
    /// needs a real error channel anyway.
    pub fn pump_notices(&self) -> usize {
        let drained = self.drain_notices();
        if drained.is_empty() {
            return 0;
        }
        for notice in &drained {
            if notice.level == NoticeLevel::Error {
                eprintln!("storage error: {}", notice.text);
            }
        }
        // Highest severity wins; the *last* index of that severity keeps
        // the newest among equals.
        let winner = drained
            .iter()
            .enumerate()
            .max_by_key(|(i, n)| (n.level.rank(), *i))
            .map(|(_, n)| n);
        if let Some(winner) = winner {
            let mut slot = lock(&self.last_notice);
            let displacing_something_louder = matches!(
                slot.as_ref(),
                Some(shown) if shown.level.rank() > winner.level.rank()
            );
            if !displacing_something_louder {
                *slot = Some(winner.clone());
            }
        }
        // The status bar's text just changed, and this runs outside event
        // dispatch, so nothing else will invalidate it.
        agg_gui::animation::request_draw();
        drained.len()
    }

    /// The message currently on display, if any.
    pub fn last_notice(&self) -> Option<Notice> {
        lock(&self.last_notice).clone()
    }

    /// Clear the displayed message — the status bar calls this when the
    /// user clicks the text.
    pub fn dismiss_notice(&self) {
        *lock(&self.last_notice) = None;
        agg_gui::animation::request_draw();
    }

    /// The status bar's storage readout: the first in-flight operation's
    /// label, its progress when the provider reports any, and how many
    /// other operations are queued behind it. `None` when nothing is in
    /// flight, in which case the status bar reserves no space at all.
    ///
    /// Lives here rather than in the widget so tests can assert on the
    /// exact string the widget paints without reading pixels.
    pub fn storage_activity_text(&self) -> Option<String> {
        let ops = self.pending_op_status();
        let (label, progress) = ops.first()?;
        let mut text = format!("{label}…");
        if let Some(progress) = progress {
            text.push_str(&format!(" {}%", (progress * 100.0).round() as i64));
        }
        if ops.len() > 1 {
            text.push_str(&format!(" (+{} more)", ops.len() - 1));
        }
        Some(text)
    }

    /// Hand a storage operation to the frame pump.
    ///
    /// The job is polled once first: a synchronous provider has already
    /// settled it, so the continuation runs *inline* and the queue is never
    /// touched. That keeps the desktop path exactly as immediate as it is
    /// today — no operation waits a frame just because the machinery could
    /// have.
    ///
    /// Anything still pending is parked and a redraw is requested, because
    /// agg-gui's reactive loop would otherwise go to sleep and never call
    /// [`Self::pump_storage`] again.
    ///
    /// # Re-entrancy contract
    ///
    /// The continuation runs **synchronously on the caller's stack**
    /// whenever the job is already settled — which is every operation on
    /// every local provider. Two rules follow:
    ///
    /// - **Callers must not hold any [`AppState`] lock across
    ///   `submit_op`.** Handing in an operation while holding, say,
    ///   `state.graph` self-deadlocks the instant the continuation tries
    ///   to lock it. Take what you need, drop the guard, then submit.
    /// - **Continuations must tolerate either timing**: running inline
    ///   during event handling, or a frame (or many) later from
    ///   [`Self::pump_storage`]. They must not assume the state they saw
    ///   at submit time is still current, and must not assume any
    ///   particular lock is free or held.
    ///
    /// That inline path is also why this is **not** unconditionally
    /// any-thread: parking a still-pending job is, but running a
    /// continuation is main-thread work. Call it on the main thread
    /// unless you know the job cannot already be settled.
    pub fn submit_op(&self, op: Box<dyn PendingOp>) {
        if op.poll().is_settled() {
            op.apply(self);
            // The continuation just changed something the user can see;
            // the pending-op branch below is not the only path that owes
            // the window a paint.
            agg_gui::animation::signal_async_state_change();
            return;
        }
        lock(&self.pending_ops).push(op);
        // Cross-thread-safe: a worker that submits an operation must be
        // able to wake the main loop, which a thread-local flag cannot do.
        agg_gui::animation::signal_async_state_change();
    }

    /// Apply every settled operation. Called once per frame by each shell,
    /// **before** its paint gate, so a job that landed on an otherwise idle
    /// frame is still applied.
    ///
    /// Returns `true` when operations remain in flight, and in that case
    /// requests another draw so the reactive loop keeps ticking and calls
    /// back here next frame.
    ///
    /// A draw is *also* requested whenever at least one continuation ran,
    /// even if that emptied the queue: the settling frame is precisely the
    /// frame that swaps in a loaded graph or a new title, and without the
    /// request the window would keep showing the old content until the
    /// next input event.
    ///
    /// The queue mutex is *not* held while a continuation runs: a
    /// continuation is allowed to call [`Self::submit_op`] itself (a chained
    /// operation), and re-entering the lock would deadlock.
    pub fn pump_storage(&self) -> bool {
        // Ahead of the early return below: an inline continuation posts
        // its message without ever queueing an operation, so notices must
        // drain even on frames where the pending queue is empty.
        self.pump_notices();
        let settled: Vec<Box<dyn PendingOp>> = {
            let mut ops = lock(&self.pending_ops);
            if ops.is_empty() {
                return false;
            }
            let (settled, still_pending) = std::mem::take(&mut *ops)
                .into_iter()
                .partition(|op| op.poll().is_settled());
            *ops = still_pending;
            settled
        };
        let applied = !settled.is_empty();
        for op in settled {
            op.apply(self);
        }
        // Re-read rather than reusing the count above: a continuation may
        // have queued a follow-up operation while the lock was released.
        let pending = !lock(&self.pending_ops).is_empty();
        if applied {
            // A continuation swapped in a loaded graph or a new title;
            // that is a content change, so invalidate properly.
            agg_gui::animation::request_draw();
        } else if pending {
            // Nothing changed. Whether that is worth a frame depends on
            // what is queued — see [`crate::storage_wakeup`].
            self.request_pending_wakeup();
        }
        pending
    }

    /// How many *loud* operations are waiting on their job right now — the
    /// "is storage busy?" question the status bar and the File menu ask.
    ///
    /// Quiet background work (thumbnail reads) is deliberately not counted:
    /// a directory of previews loading must not make File → Open report
    /// that storage is busy. [`Self::pending_op_count_all`] is the total.
    pub fn pending_op_count(&self) -> usize {
        lock(&self.pending_ops)
            .iter()
            .filter(|op| !op.is_quiet())
            .count()
    }

    /// Every operation on the queue, quiet ones included — the shutdown
    /// path's and a diagnostic's view of the truth.
    pub fn pending_op_count_all(&self) -> usize {
        lock(&self.pending_ops).len()
    }

    /// Label + optional progress (`0.0..=1.0`) of each in-flight *loud*
    /// operation, for the status bar's activity readout.
    pub fn pending_op_status(&self) -> Vec<(String, Option<f32>)> {
        self.op_status(false)
    }

    /// The same readout over *every* queued operation, quiet ones
    /// included. Not for the status bar — this is what a diagnostic wants
    /// when it has to name what is still outstanding (the test harness's
    /// "still pending after N frames" panic), where an empty list because
    /// the only stragglers were background reads is exactly the wrong
    /// answer.
    pub fn pending_op_status_all(&self) -> Vec<(String, Option<f32>)> {
        self.op_status(true)
    }

    fn op_status(&self, include_quiet: bool) -> Vec<(String, Option<f32>)> {
        lock(&self.pending_ops)
            .iter()
            .filter(|op| include_quiet || !op.is_quiet())
            .map(|op| {
                let progress = match op.poll() {
                    JobState::Pending { progress } => progress,
                    _ => Some(1.0),
                };
                (op.label().to_string(), progress)
            })
            .collect()
    }

    /// Cancel everything in flight — the status bar's cancel affordance
    /// and the app-shutdown path.
    ///
    /// The operations stay queued: a cancelled job settles as
    /// [`StorageError::Cancelled`], so the next pump applies each
    /// continuation with that error and the app gets to react (drop the
    /// half-loaded project, tell the user the save was abandoned) exactly
    /// as it would for any other failure.
    pub fn cancel_pending_ops(&self) {
        for op in lock(&self.pending_ops).iter() {
            op.cancel();
        }
        agg_gui::animation::request_draw();
    }

    /// Run the pump until the queue drains or `timeout` elapses — the
    /// app-shutdown path.
    ///
    /// Without this, closing the window between the submit and the settle
    /// of a save silently discards it: the event loop stops calling
    /// [`Self::pump_storage`] and the continuation that would have
    /// re-baselined (or reported the failure) never runs.
    ///
    /// Returns `true` when everything drained. On timeout it cancels
    /// whatever is left and pumps — a cancelled job settles as
    /// [`StorageError::Cancelled`], so continuations still observe an
    /// outcome — reports the abandoned labels on stderr, and returns
    /// `false`.
    ///
    /// **Quiet operations are not waited for.** Once only background work
    /// is left (thumbnail reads), the drain cancels it and returns `true`:
    /// nothing is lost by abandoning an image the window is about to stop
    /// showing, and a slow provider must not hold the close open for one.
    ///
    /// That cancel-and-pump repeats up to [`FINAL_PUMP_ROUNDS`] times,
    /// because a continuation is allowed to chain a follow-up operation
    /// ("save, then verify"): a single final pump would leave the
    /// follow-up queued and drop it silently, which is exactly the failure
    /// this method exists to prevent. The bound keeps a continuation that
    /// re-submits forever from wedging the close.
    ///
    /// Native-only: the browser has no exit path to drain on, and
    /// blocking its single thread would stop the very worker the jobs
    /// depend on.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn drain_pending_ops(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if !self.pump_storage() {
                return true;
            }
            if self.pending_op_count() == 0 {
                // Only quiet work left. Cancel it so its continuations see
                // an outcome, pump those through, and go — the same
                // bounded rounds the timeout path uses, because a quiet
                // continuation may chain as freely as any other.
                for _ in 0..FINAL_PUMP_ROUNDS {
                    self.cancel_pending_ops();
                    if !self.pump_storage() {
                        break;
                    }
                }
                // A provider that ignores cancel leaves them behind. The
                // close still succeeds — nothing the user asked for is at
                // stake — but the shutdown path reports everything it
                // walks away from, quiet work included.
                let left_over: Vec<String> = self
                    .pending_op_status_all()
                    .into_iter()
                    .map(|(label, _progress)| label)
                    .collect();
                if !left_over.is_empty() {
                    eprintln!(
                        "storage: {} background operation(s) dropped at exit after \
                         {FINAL_PUMP_ROUNDS} cancel rounds: {}",
                        left_over.len(),
                        left_over.join(", ")
                    );
                }
                return true;
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            // Providers settle on their own threads; yield rather than
            // spin so they get the CPU.
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        let abandoned: Vec<String> = self
            .pending_op_status()
            .into_iter()
            .map(|(label, _progress)| label)
            .collect();
        // Let the continuations observe `Cancelled` before we walk away —
        // including any follow-up operation they chain while doing so.
        for _ in 0..FINAL_PUMP_ROUNDS {
            self.cancel_pending_ops();
            if !self.pump_storage() {
                break;
            }
        }
        eprintln!(
            "storage: abandoned {} operation(s) after waiting {:?} at exit: {}",
            abandoned.len(),
            timeout,
            abandoned.join(", ")
        );
        let left_over = self.pending_op_status();
        if !left_over.is_empty() {
            let labels: Vec<String> = left_over.into_iter().map(|(l, _)| l).collect();
            eprintln!(
                "storage: {} operation(s) still queued after {FINAL_PUMP_ROUNDS} \
                 cancel rounds and dropped at exit: {}",
                labels.len(),
                labels.join(", ")
            );
        }
        false
    }
}

// Tests live in `storage_ops_tests.rs` so this file stays under the
// 800-line cap.
#[cfg(test)]
#[path = "storage_ops_tests.rs"]
mod storage_ops_tests;

// The quiet-operation tests live in their own file for the same reason.
#[cfg(test)]
#[path = "storage_ops_quiet_tests.rs"]
mod storage_ops_quiet_tests;

// …as do the notice-queue de-duplication tests.
#[cfg(test)]
#[path = "storage_ops_notice_tests.rs"]
mod storage_ops_notice_tests;
