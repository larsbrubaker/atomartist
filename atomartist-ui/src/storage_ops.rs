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
//! A continuation runs outside the widget tree, so it has no dialog
//! provider to talk to. Instead it leaves a [`Notice`] on the state
//! ([`AppState::notify`]) and the UI drains it
//! ([`AppState::drain_notices`]) when it next paints.
//!
//! Synchronous providers (`LocalFsProvider`, `MemoryProvider`) settle their
//! jobs before `submit_op` returns, so `submit_op` applies those inline and
//! the desktop path never waits a frame — the pump exists for the providers
//! that cannot do that.
//!
//! Known deferred items, for Phase 4b to pick up:
//!
//! 1. The keep-alive uses `agg_gui::animation::request_draw`, which bumps
//!    the invalidation epoch on every frame an operation is in flight —
//!    a long download would re-layout the whole tree each frame for no
//!    reason. `request_draw_without_invalidation` is the likely fix, but
//!    per CLAUDE.md it needs a measurement before it counts as one.
//! 2. [`AppState::notify`] and [`AppState::submit_op`] are main-thread
//!    only, because agg-gui's draw-request flag is thread-local: calling
//!    them from a worker sets a flag nobody reads. A cross-thread wakeup
//!    (`agg_gui::animation::signal_async_state_change`, or an
//!    `EventLoopProxy`) lands in 4b alongside the status-bar readout.
//!
//! Relationship to [`crate::app_state_storage`]: that module's `await_job`
//! is the *synchronous-only* bridge the current call sites in
//! [`crate::app_state_files`] still use. This module is its asynchronous
//! successor; the call sites move across in a later step.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use atomartist_storage::{Job, JobState, StorageError};

use crate::app_state::AppState;

/// How prominently a [`Notice`] should be shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    /// Progress / confirmation text — the status bar is enough.
    Info,
    /// Something failed and the user needs to know.
    Error,
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

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl AppState {
    /// Post a message for the UI to show on its next paint.
    pub fn notify(&self, level: NoticeLevel, text: impl Into<String>) {
        lock(&self.notices).push(Notice {
            level,
            text: text.into(),
        });
        agg_gui::animation::request_draw();
    }

    /// Take every queued message, leaving the queue empty.
    pub fn drain_notices(&self) -> Vec<Notice> {
        std::mem::take(&mut *lock(&self.notices))
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
    pub fn submit_op(&self, op: Box<dyn PendingOp>) {
        if op.poll().is_settled() {
            op.apply(self);
            // The continuation just changed something the user can see;
            // the pending-op branch below is not the only path that owes
            // the window a paint.
            agg_gui::animation::request_draw();
            return;
        }
        lock(&self.pending_ops).push(op);
        agg_gui::animation::request_draw();
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
        if applied || pending {
            agg_gui::animation::request_draw();
        }
        pending
    }

    /// How many operations are waiting on their job right now.
    pub fn pending_op_count(&self) -> usize {
        lock(&self.pending_ops).len()
    }

    /// Label + optional progress (`0.0..=1.0`) of each in-flight
    /// operation, for the status bar's activity readout.
    pub fn pending_op_status(&self) -> Vec<(String, Option<f32>)> {
        lock(&self.pending_ops)
            .iter()
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use atomartist_storage::{
        FlakyConfig, FlakyProvider, MemoryProvider, Precondition, StorageProvider, StorageRegistry,
        StorageUri,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn state() -> AppState {
        AppState::new(
            atomartist_lib::Graph::new(),
            atomartist_lib::registry::NodeRegistry::new(),
        )
    }

    /// A memory provider wrapped in `latency` pump-ticks of simulated
    /// delay, plus the URI of its root.
    fn flaky(config: FlakyConfig) -> (Arc<FlakyProvider>, StorageUri) {
        let inner = MemoryProvider::new("mem", "Memory");
        let root = inner.root();
        (Arc::new(FlakyProvider::new(Arc::new(inner), config)), root)
    }

    fn sync_registry() -> Arc<StorageRegistry> {
        let mut registry = StorageRegistry::new();
        registry
            .register(Arc::new(MemoryProvider::new("mem", "Memory")))
            .expect("fresh registry");
        Arc::new(registry)
    }

    /// The desktop path: a provider that settles immediately must not cost
    /// a frame, so `submit_op` applies the continuation before it returns.
    #[test]
    fn a_synchronous_op_is_applied_inline_at_submit_time() {
        let registry = sync_registry();
        let state = AppState::with_storage(
            atomartist_lib::Graph::new(),
            atomartist_lib::registry::NodeRegistry::new(),
            registry.clone(),
        );
        let uri: StorageUri = "mem:///a.bin".parse().unwrap();
        let provider = registry.resolve(&uri).expect("memory provider");
        provider
            .write(&uri, b"payload".to_vec(), Precondition::None)
            .take()
            .expect("synchronous write settles")
            .expect("write succeeds");

        let seen = Arc::new(Mutex::new(None::<Vec<u8>>));
        let sink = seen.clone();
        state.submit_op(Box::new(JobOp::new(
            "Opening a.bin",
            provider.read(&uri),
            move |_state, result| {
                *lock(&sink) = result.ok();
            },
        )));

        assert_eq!(state.pending_op_count(), 0, "nothing should be queued");
        assert_eq!(lock(&seen).as_deref(), Some(&b"payload"[..]));
    }

    /// The whole point of the pump: a provider that needs time gets its
    /// continuation run on a later frame, exactly once.
    #[test]
    fn a_delayed_op_is_parked_until_the_pump_sees_it_settle() {
        let state = state();
        let (provider, root) = flaky(FlakyConfig::default().with_latency(2));
        let at = root.join("a.bin");
        provider
            .write(&at, b"payload".to_vec(), Precondition::None)
            .poll();
        provider.pump_until_idle();

        let runs = Arc::new(AtomicUsize::new(0));
        let counter = runs.clone();
        state.submit_op(Box::new(JobOp::new(
            "Opening a.bin",
            provider.read(&at),
            move |_state, result| {
                assert_eq!(result.expect("read succeeds"), b"payload".to_vec());
                counter.fetch_add(1, Ordering::Relaxed);
            },
        )));

        assert_eq!(state.pending_op_count(), 1, "latency parks the op");
        assert_eq!(
            state.pending_op_status(),
            vec![("Opening a.bin".to_string(), None)]
        );

        // Frame 1: the provider's clock has not delivered yet.
        provider.pump();
        assert!(state.pump_storage(), "still in flight after one tick");
        assert_eq!(runs.load(Ordering::Relaxed), 0);

        // Frame 2: the result lands and the continuation runs.
        provider.pump();
        assert!(!state.pump_storage(), "queue drains on the settling frame");
        assert_eq!(runs.load(Ordering::Relaxed), 1);
        assert_eq!(state.pending_op_count(), 0);

        // Further frames must not re-run it.
        state.pump_storage();
        assert_eq!(runs.load(Ordering::Relaxed), 1);
    }

    /// Chained operations are the reason `pump_storage` releases the queue
    /// lock before applying: without that this test deadlocks.
    #[test]
    fn a_continuation_can_submit_another_op_without_deadlocking() {
        let state = state();
        let (provider, root) = flaky(FlakyConfig::default().with_latency(1));
        let at = root.join("a.bin");
        let child_provider = provider.clone();
        let child_at = at.clone();

        let child_done = Arc::new(AtomicUsize::new(0));
        let child_counter = child_done.clone();
        state.submit_op(Box::new(JobOp::new(
            "Saving a.bin",
            provider.write(&at, b"payload".to_vec(), Precondition::None),
            move |state, result| {
                result.expect("write succeeds");
                let counter = child_counter.clone();
                state.submit_op(Box::new(JobOp::new(
                    "Verifying a.bin",
                    child_provider.read(&child_at),
                    move |_state, result| {
                        assert_eq!(result.expect("read back"), b"payload".to_vec());
                        counter.fetch_add(1, Ordering::Relaxed);
                    },
                )));
            },
        )));

        provider.pump();
        assert!(
            state.pump_storage(),
            "the child op queued by the parent keeps the pump alive"
        );
        assert_eq!(state.pending_op_count(), 1);

        provider.pump();
        assert!(!state.pump_storage());
        assert_eq!(child_done.load(Ordering::Relaxed), 1);
    }

    /// A failure reaches the continuation as `Err`, and the continuation's
    /// message reaches the UI through the notice queue.
    #[test]
    fn a_failed_op_delivers_its_error_and_can_post_a_notice() {
        let state = state();
        let (provider, root) = flaky(FlakyConfig::default().with_latency(1));
        let at = root.join("missing.bin");

        state.submit_op(Box::new(JobOp::new(
            "Opening missing.bin",
            provider.read(&at),
            |state, result| {
                let err = result.err().expect("reading a missing file fails");
                assert_eq!(err, StorageError::NotFound);
                state.notify(NoticeLevel::Error, format!("could not open: {err}"));
            },
        )));

        // The failure travels through the pump exactly like a success.
        assert_eq!(state.pending_op_count(), 1);
        provider.pump();
        assert!(!state.pump_storage());
        assert_eq!(state.pending_op_count(), 0);
        let notices = state.drain_notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].level, NoticeLevel::Error);
        assert!(
            notices[0].text.starts_with("could not open:"),
            "unexpected notice: {}",
            notices[0].text
        );
        assert!(state.drain_notices().is_empty(), "draining is destructive");
    }

    #[test]
    fn cancelling_settles_pending_ops_as_cancelled() {
        let state = state();
        let (provider, root) = flaky(FlakyConfig::default().with_latency(5));
        let at = root.join("a.bin");

        let seen = Arc::new(Mutex::new(None::<StorageError>));
        let sink = seen.clone();
        state.submit_op(Box::new(JobOp::new(
            "Opening a.bin",
            provider.read(&at),
            move |_state, result| {
                *lock(&sink) = result.err();
            },
        )));
        assert_eq!(state.pending_op_count(), 1);

        state.cancel_pending_ops();
        // Cancellation settles the job, so the very next pump — with no
        // further provider ticks — applies the continuation.
        assert!(!state.pump_storage());
        assert_eq!(*lock(&seen), Some(StorageError::Cancelled));
        assert_eq!(state.pending_op_count(), 0);
    }

    /// Applying a continuation changes what the window shows (a loaded
    /// graph swaps in, a title updates), so the frame that applies it must
    /// also ask for a paint. The keep-alive request only covers frames
    /// where work *remains*; on the settling frame there is nothing left
    /// to keep alive and the window would otherwise stay stale until the
    /// next input event.
    #[test]
    fn the_pump_requests_a_draw_on_the_frame_it_applies_an_op() {
        let state = state();
        let (provider, root) = flaky(FlakyConfig::default().with_latency(1));
        let at = root.join("a.bin");

        let runs = Arc::new(AtomicUsize::new(0));
        let counter = runs.clone();
        state.submit_op(Box::new(JobOp::new(
            "Saving a.bin",
            provider.write(&at, b"payload".to_vec(), Precondition::None),
            move |_state, result| {
                result.expect("write succeeds");
                counter.fetch_add(1, Ordering::Relaxed);
            },
        )));

        provider.pump();
        // Clear *after* the provider tick so only the pump's own request
        // can set the flag. The continuation deliberately posts no notice,
        // because `notify` requests a draw of its own.
        agg_gui::animation::clear_draw_request();
        assert!(!state.pump_storage(), "the queue drains on this frame");
        assert_eq!(runs.load(Ordering::Relaxed), 1, "continuation ran");
        assert!(
            agg_gui::animation::wants_draw(),
            "applying a continuation must repaint the window"
        );
    }

    /// The inline fast path has the same obligation: a synchronous
    /// provider's continuation runs during event handling, and whatever it
    /// changed has to reach the screen.
    #[test]
    fn an_inline_op_requests_a_draw() {
        let registry = sync_registry();
        let state = AppState::with_storage(
            atomartist_lib::Graph::new(),
            atomartist_lib::registry::NodeRegistry::new(),
            registry.clone(),
        );
        let uri: StorageUri = "mem:///a.bin".parse().unwrap();
        let provider = registry.resolve(&uri).expect("memory provider");

        let runs = Arc::new(AtomicUsize::new(0));
        let counter = runs.clone();
        let job = provider.write(&uri, b"payload".to_vec(), Precondition::None);
        agg_gui::animation::clear_draw_request();
        state.submit_op(Box::new(JobOp::new(
            "Saving a.bin",
            job,
            move |_state, result| {
                result.expect("write succeeds");
                counter.fetch_add(1, Ordering::Relaxed);
            },
        )));

        assert_eq!(state.pending_op_count(), 0, "applied inline");
        assert_eq!(runs.load(Ordering::Relaxed), 1);
        assert!(
            agg_gui::animation::wants_draw(),
            "an inline continuation must repaint the window"
        );
    }

    /// Documents the re-entrancy contract of [`AppState::submit_op`]: the
    /// continuation may run on the caller's own stack, so it may take any
    /// `AppState` lock *provided the caller holds none*. The mirror case —
    /// caller holding `state.graph` across `submit_op` — self-deadlocks and
    /// is therefore a rule in the doc comment rather than a test.
    #[test]
    fn an_inline_continuation_may_lock_app_state() {
        let registry = sync_registry();
        let state = AppState::with_storage(
            atomartist_lib::Graph::new(),
            atomartist_lib::registry::NodeRegistry::new(),
            registry.clone(),
        );
        let uri: StorageUri = "mem:///a.bin".parse().unwrap();
        let provider = registry.resolve(&uri).expect("memory provider");

        let node_count = Arc::new(AtomicUsize::new(usize::MAX));
        let sink = node_count.clone();
        state.submit_op(Box::new(JobOp::new(
            "Saving a.bin",
            provider.write(&uri, b"payload".to_vec(), Precondition::None),
            move |state, result| {
                result.expect("write succeeds");
                // Runs inline, on this very stack. Legal because the
                // caller above holds no lock.
                let graph = lock(&state.graph);
                sink.store(graph.nodes().count(), Ordering::Relaxed);
            },
        )));

        assert_eq!(node_count.load(Ordering::Relaxed), 0, "continuation ran");
    }

    /// The common case, run every single frame: an empty queue must be
    /// cheap and must not ask for another frame, or the app would never
    /// go idle.
    #[test]
    fn pumping_an_empty_queue_is_a_no_op() {
        let state = state();
        agg_gui::animation::clear_draw_request();
        assert!(!state.pump_storage());
        assert!(
            !agg_gui::animation::wants_draw(),
            "an idle pump must not keep the reactive loop awake"
        );
    }
}
