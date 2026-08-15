//! Unit tests for [`crate::storage_ops`] — the frame-loop job pump, the
//! notice queue, and the shutdown drain.
//!
//! Split out of `storage_ops.rs` to keep both files under the 800-line cap
//! (CLAUDE.md). Re-attached via `#[path]` from the parent module, so
//! `use super::*` still reaches its private items.

use super::*;
use atomartist_storage::{
    FlakyConfig, FlakyProvider, MemoryProvider, Precondition, StorageProvider, StorageRegistry,
    StorageUri,
};
use std::sync::atomic::{AtomicUsize, Ordering};

pub(super) fn state() -> AppState {
    AppState::new(
        atomartist_lib::Graph::new(),
        atomartist_lib::registry::NodeRegistry::new(),
    )
}

/// A memory provider wrapped in `latency` pump-ticks of simulated
/// delay, plus the URI of its root.
pub(super) fn flaky(config: FlakyConfig) -> (Arc<FlakyProvider>, StorageUri) {
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
///
/// `wants_draw` merges a process-global counter that any other test
/// thread may bump via `signal_async_state_change`, so one observation
/// can be perturbed by unrelated tests. Same treatment as
/// [`the_keep_alive_does_not_advance_the_invalidation_epoch`]: the pass
/// condition is that at least one attempt sees an idle flag, which no
/// amount of interference can prevent from eventually happening.
#[test]
fn pumping_an_empty_queue_is_a_no_op() {
    let state = state();
    let mut saw_idle = false;
    for _ in 0..5 {
        agg_gui::animation::clear_draw_request();
        assert!(!state.pump_storage());
        if !agg_gui::animation::wants_draw() {
            saw_idle = true;
            break;
        }
    }
    assert!(
        saw_idle,
        "an idle pump must not keep the reactive loop awake"
    );
}

// ── 4b: notices, status text, shutdown drain ─────────────────────────────

/// An embedder that never drains must not leak: past [`NOTICE_CAP`] the
/// oldest messages are dropped and the newest survive.
#[test]
fn the_notice_queue_is_capped() {
    let state = state();
    for i in 0..(NOTICE_CAP + 50) {
        state.notify(NoticeLevel::Info, format!("notice {i}"));
    }
    let drained = state.drain_notices();
    assert_eq!(drained.len(), NOTICE_CAP, "queue is capped");
    assert_eq!(
        drained.last().map(|n| n.text.clone()),
        Some(format!("notice {}", NOTICE_CAP + 49)),
        "the newest message is kept"
    );
    assert_eq!(
        drained.first().map(|n| n.text.as_str()),
        Some("notice 50"),
        "the oldest messages are the ones dropped"
    );
}

/// The status bar's message slot: the newest notice wins among equals, an
/// error sticks until another error replaces it or the user dismisses it,
/// and a click dismisses it.
#[test]
fn pumping_notices_parks_the_newest_for_display() {
    let state = state();
    assert_eq!(state.last_notice(), None, "nothing to show on a fresh state");

    state.notify(NoticeLevel::Error, "could not open: not found");
    assert_eq!(state.pump_notices(), 1);
    assert_eq!(
        state.last_notice(),
        Some(Notice {
            level: NoticeLevel::Error,
            text: "could not open: not found".to_string(),
        })
    );

    // Nothing new posted: the error stays on screen.
    assert_eq!(state.pump_notices(), 0);
    assert!(matches!(
        state.last_notice(),
        Some(Notice {
            level: NoticeLevel::Error,
            ..
        })
    ));

    // A newer error replaces an older one.
    state.notify(NoticeLevel::Error, "could not save: read only");
    state.pump_notices();
    assert_eq!(
        state.last_notice().map(|n| (n.level, n.text)),
        Some((
            NoticeLevel::Error,
            "could not save: read only".to_string()
        ))
    );

    state.dismiss_notice();
    assert_eq!(state.last_notice(), None, "clicking the text clears it");

    // With the slot clear, an info notice takes it.
    state.notify(NoticeLevel::Info, "Saved bracket.atmr");
    state.pump_notices();
    assert_eq!(
        state.last_notice().map(|n| (n.level, n.text)),
        Some((NoticeLevel::Info, "Saved bracket.atmr".to_string()))
    );

    // And a newer info replaces an older info.
    state.notify(NoticeLevel::Info, "Saved other.atmr");
    state.pump_notices();
    assert_eq!(
        state.last_notice().map(|n| n.text),
        Some("Saved other.atmr".to_string())
    );
}

/// Bug (a): a single drain batch holding `[Error, Info]` used to show only
/// the Info, because the newest message won unconditionally. A failed save
/// hiding behind the *next* operation's confirmation is the worst failure
/// mode this seam has, so severity outranks recency inside a batch.
#[test]
fn an_error_in_a_batch_outranks_a_newer_info() {
    let state = state();
    state.notify(NoticeLevel::Error, "could not save: disk full");
    state.notify(NoticeLevel::Info, "Saved b.atmr");
    assert_eq!(state.pump_notices(), 2, "both messages drained");
    assert_eq!(
        state.last_notice().map(|n| (n.level, n.text)),
        Some((
            NoticeLevel::Error,
            "could not save: disk full".to_string()
        )),
        "the error must win the display slot over a newer info"
    );
}

/// Among equal severities the newest still wins — so two errors in one
/// batch show the later one.
#[test]
fn the_newest_error_wins_among_equal_severities() {
    let state = state();
    state.notify(NoticeLevel::Error, "first failure");
    state.notify(NoticeLevel::Info, "progress");
    state.notify(NoticeLevel::Error, "second failure");
    state.pump_notices();
    assert_eq!(
        state.last_notice().map(|n| n.text),
        Some("second failure".to_string())
    );
}

/// Bug (b): an error already on display must not be silently replaced by
/// a later, quieter notice from a *different* pump. It stays until another
/// error replaces it or the user dismisses it.
#[test]
fn an_info_does_not_replace_an_undismissed_error() {
    let state = state();
    state.notify(NoticeLevel::Error, "could not save: disk full");
    state.pump_notices();

    state.notify(NoticeLevel::Info, "Saved b.atmr");
    assert_eq!(state.pump_notices(), 1, "the info was drained");
    assert_eq!(
        state.last_notice().map(|n| (n.level, n.text)),
        Some((
            NoticeLevel::Error,
            "could not save: disk full".to_string()
        )),
        "an undismissed error outlives a later info"
    );

    // Dismissal clears the slot for anything — including an info.
    state.dismiss_notice();
    state.notify(NoticeLevel::Info, "Saved c.atmr");
    state.pump_notices();
    assert_eq!(
        state.last_notice().map(|n| (n.level, n.text)),
        Some((NoticeLevel::Info, "Saved c.atmr".to_string())),
        "dismissal frees the slot for a quieter notice"
    );
}

/// The wakeup `notify` uses has to be the *cross-thread-visible* one.
///
/// The literal test — spawn a thread, call `state.notify(..)`, assert the
/// main thread's `wants_draw()` — cannot be written today: [`AppState`] is
/// `!Send` (agg-gui's `UndoBuffer` holds `Box<dyn UndoRedoCommand>` with no
/// `Send` bound), so no worker can hold one. What *is* observable here is
/// the property that makes the cross-thread merge work at all:
/// `signal_async_state_change` bumps the process-global wakeup counter and
/// the async-state epoch, while a plain `request_draw` bumps neither. If
/// this ever reverts to `request_draw`, the epoch stops advancing and this
/// test fails.
#[test]
fn notify_uses_the_cross_thread_wakeup() {
    let state = state();
    agg_gui::animation::clear_draw_request();
    let before = agg_gui::animation::async_state_epoch();
    state.notify(NoticeLevel::Info, "Saved a.atmr");
    assert!(
        agg_gui::animation::async_state_epoch() != before,
        "notify must signal the async-state change, not just request a draw"
    );
    assert!(
        agg_gui::animation::wants_draw(),
        "a posted notice must bring the loop back"
    );
}

/// Same obligation for `submit_op` on a genuinely pending job: the
/// operation is parked and only a later pump can apply it, so the loop has
/// to be woken through the cross-thread channel.
#[test]
fn submitting_a_pending_op_uses_the_cross_thread_wakeup() {
    let state = state();
    let (provider, root) = flaky(FlakyConfig::default().with_latency(1000));
    let at = root.join("a.bin");

    agg_gui::animation::clear_draw_request();
    let before = agg_gui::animation::async_state_epoch();
    state.submit_op(Box::new(JobOp::new(
        "Opening a.bin",
        provider.read(&at),
        |_state, _result| {},
    )));

    assert_eq!(state.pending_op_count(), 1, "the op is genuinely pending");
    assert!(
        agg_gui::animation::async_state_epoch() != before,
        "submit_op must signal the async-state change"
    );
    assert!(
        agg_gui::animation::wants_draw(),
        "a parked op must bring the loop back"
    );

    state.cancel_pending_ops();
    state.pump_storage();
}

/// `pump_storage` is the once-per-frame call every shell makes, so it —
/// not just a direct `pump_notices` — must service the queue. Critically
/// it has to do so *before* its empty-queue early return, or a message
/// from an inline (synchronous-provider) continuation would never show.
#[test]
fn pump_storage_drains_notices_even_with_an_empty_queue() {
    let state = state();
    state.notify(NoticeLevel::Info, "Opened a.atmr");
    assert_eq!(state.pending_op_count(), 0);
    assert!(!state.pump_storage());
    assert_eq!(
        state.last_notice().map(|n| n.text),
        Some("Opened a.atmr".to_string())
    );
}

/// The exact strings the status bar paints. Asserted here rather than
/// against pixels, and the widget calls this same function.
#[test]
fn the_storage_activity_text_reports_label_progress_and_backlog() {
    let state = state();
    assert_eq!(state.storage_activity_text(), None, "idle shows nothing");

    let (provider, root) = flaky(FlakyConfig::default().with_latency(5));
    state.submit_op(Box::new(JobOp::new(
        "Opening bracket.atmr",
        provider.read(&root.join("bracket.atmr")),
        |_state, _result| {},
    )));
    assert_eq!(
        state.storage_activity_text().as_deref(),
        Some("Opening bracket.atmr…"),
        "a provider that reports no progress shows none"
    );

    state.submit_op(Box::new(JobOp::new(
        "Saving other.atmr",
        provider.write(&root.join("other.atmr"), b"x".to_vec(), Precondition::None),
        |_state, _result| {},
    )));
    assert_eq!(
        state.storage_activity_text().as_deref(),
        Some("Opening bracket.atmr… (+1 more)"),
        "the first op leads, the rest are counted"
    );
}

/// Progress, when a provider reports it, renders as a percentage.
#[test]
fn the_storage_activity_text_renders_progress_as_a_percentage() {
    let state = state();
    let (job, completer) = atomartist_storage::Job::<Vec<u8>>::pending();
    completer.set_progress(Some(0.42));
    state.submit_op(Box::new(JobOp::new(
        "Opening bracket.atmr",
        job,
        |_state, _result| {},
    )));
    assert_eq!(
        state.storage_activity_text().as_deref(),
        Some("Opening bracket.atmr… 42%")
    );
    // Hold the completer until after the assertion: dropping it settles
    // the job (as a failure), which would empty the readout.
    drop(completer);
}

/// Evidence for the keep-alive decision (module docs, "Keep-alive cost"):
/// a frame that only keeps the loop ticking must ask for a draw *without*
/// advancing the invalidation epoch.
///
/// The keep-alive only fires for an operation that is reporting progress
/// (see [`crate::storage_wakeup`]), so the job here is held pending with a
/// percentage on it rather than merely being slow.
///
/// `invalidation_epoch` merges a process-global counter that any other
/// test thread may bump via `signal_async_state_change`, so a single
/// observation could be perturbed by unrelated tests; the pass condition
/// is that at least one attempt sees an unchanged epoch, which no amount
/// of interference can prevent from happening eventually.
#[test]
fn the_keep_alive_does_not_advance_the_invalidation_epoch() {
    let state = state();
    let (job, completer) = atomartist_storage::Job::<Vec<u8>>::pending();
    completer.set_progress(Some(0.5));
    state.submit_op(Box::new(JobOp::new(
        "Opening a.bin",
        job,
        |_state, _result| {},
    )));

    let mut saw_stable_epoch = false;
    for _ in 0..5 {
        agg_gui::animation::clear_draw_request();
        let before = agg_gui::animation::invalidation_epoch();
        assert!(state.pump_storage(), "the op is still in flight");
        assert!(
            agg_gui::animation::wants_draw(),
            "the keep-alive must still bring the loop back next frame"
        );
        if agg_gui::animation::invalidation_epoch() == before {
            saw_stable_epoch = true;
            break;
        }
    }
    assert!(
        saw_stable_epoch,
        "keep-alive frames must not bump the invalidation epoch"
    );
    drop(completer);
    state.pump_storage();
}

/// The shutdown path's happy case: work that settles while we wait is
/// applied, and the drain reports success.
#[test]
fn draining_waits_for_an_op_that_settles() {
    let state = state();
    let (provider, root) = flaky(FlakyConfig::default().with_latency(3));
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
    assert_eq!(state.pending_op_count(), 1);

    // Stand in for the provider's own worker: advance its simulated
    // clock from another thread while the drain blocks.
    let ticker = provider.clone();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_flag = stop.clone();
    let pumping = std::thread::spawn(move || {
        while !stop_flag.load(Ordering::Relaxed) {
            ticker.pump();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    });

    let drained = state.drain_pending_ops(std::time::Duration::from_secs(5));
    stop.store(true, Ordering::Relaxed);
    pumping.join().expect("ticker thread");

    assert!(drained, "the queue settled inside the deadline");
    assert_eq!(runs.load(Ordering::Relaxed), 1, "the save was applied");
    assert_eq!(state.pending_op_count(), 0);
}

/// The shutdown path's bad case: a job that never settles must not hang
/// the close. The drain gives up at the deadline, cancels, and pumps once
/// more so the continuation still observes an outcome.
#[test]
fn draining_times_out_then_cancels_and_delivers_cancelled() {
    let state = state();
    // Latency far beyond the deadline, and nothing pumps the provider —
    // this job cannot settle on its own.
    let (provider, root) = flaky(FlakyConfig::default().with_latency(1_000_000));
    let at = root.join("a.bin");

    let seen = Arc::new(Mutex::new(None::<StorageError>));
    let sink = seen.clone();
    // The continuation chains a follow-up op — the "save, then verify"
    // shape. A single final pump would leave that follow-up queued and
    // drop it on the floor, so the drain keeps cancelling and pumping
    // until the queue is actually empty.
    let chained_seen = Arc::new(Mutex::new(None::<StorageError>));
    let chained_sink = chained_seen.clone();
    let chained_provider = provider.clone();
    let chained_at = at.clone();
    state.submit_op(Box::new(JobOp::new(
        "Saving a.bin",
        provider.write(&at, b"payload".to_vec(), Precondition::None),
        move |state, result| {
            *lock(&sink) = result.err();
            let sink = chained_sink.clone();
            state.submit_op(Box::new(JobOp::new(
                "Verifying a.bin",
                chained_provider.read(&chained_at),
                move |_state, result| {
                    *lock(&sink) = result.err();
                },
            )));
        },
    )));

    let drained = state.drain_pending_ops(std::time::Duration::from_millis(50));
    assert!(!drained, "a stuck op must report the timeout");
    assert_eq!(
        *lock(&seen),
        Some(StorageError::Cancelled),
        "the continuation runs with Cancelled rather than being dropped"
    );
    assert_eq!(
        *lock(&chained_seen),
        Some(StorageError::Cancelled),
        "an op chained during the final pump must observe an outcome too"
    );
    assert_eq!(state.pending_op_count(), 0, "nothing is left behind");
}

/// An already-idle state drains instantly — the common case on close.
#[test]
fn draining_an_idle_state_returns_immediately() {
    let state = state();
    assert!(state.drain_pending_ops(std::time::Duration::from_secs(5)));
}
