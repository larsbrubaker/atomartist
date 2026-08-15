//! The process-global completion hook — how a job that settles on a
//! worker thread tells the host something happened.
//!
//! [`Job`](crate::Job) is polled, not awaited, so somebody has to look at
//! it. A host that redraws continuously (the wasm `requestAnimationFrame`
//! loop, the test harnesses) looks every tick and needs nothing from this
//! module. A *reactive* host — `demo-native`, parked in winit's
//! `ControlFlow::Wait` — is not executing at all, so a
//! `JobCompleter::succeed` on a background thread would sit unobserved
//! until an unrelated OS event happened to wake the loop. The result is a
//! save that appears to hang, or worse, one that is silently dropped at
//! exit.
//!
//! Consumers used to paper over that with a per-frame keep-alive repaint
//! for as long as anything was queued, which burns a core to stay idle
//! (measured at ~140 fps for an idle open file dialog — see
//! `docs/file-browser-design.md` step 6g-1).
//!
//! The hook closes the gap without giving this crate a GUI dependency,
//! which it deliberately does not have: the host installs one cheap,
//! thread-safe nudge at startup ([`set_completion_hook`]) and every
//! settling job calls it. `atomartist-ui` installs
//! `agg_gui::animation::signal_async_state_change`, which is exactly the
//! same shape as agg-gui's own `set_host_waker` — and, on the native
//! shell, chains into it.
//!
//! Deliberately global rather than per-job or per-registry: a completer is
//! handed to arbitrary worker code that has no reason to know about the
//! app, and the alternative is threading a callback through every provider
//! signature for a signal that is process-wide by nature.

use std::sync::{Arc, Mutex};

type CompletionHook = Arc<dyn Fn() + Send + Sync>;

/// Process-global hook slot. Locked only to clone the `Arc` out; the hook
/// itself always runs with the lock released, because it is arbitrary host
/// code and may take locks of its own.
static COMPLETION_HOOK: Mutex<Option<CompletionHook>> = Mutex::new(None);

/// Install (or replace) the process-global completion hook.
///
/// Call once from the host's startup path. Whenever any thread settles a
/// [`JobCompleter`](crate::JobCompleter) — success, failure, or the
/// dropped-without-a-result case — the hook fires *after* the result has
/// been parked in the slot, so a host woken by it is guaranteed to see the
/// settled state when it polls.
///
/// Requirements on `hook`, mirroring agg-gui's `set_host_waker`:
/// * **Cheap** — it runs inline on whatever worker thread just finished;
///   do no real work in it, just signal.
/// * **Thread-safe and non-reentrant** — it may be called from any thread
///   and must not settle another job.
/// * **Failure-tolerant** — a host that has already shut down should be
///   ignored (drop the send error), never panic.
pub fn set_completion_hook(hook: impl Fn() + Send + Sync + 'static) {
    let hook: CompletionHook = Arc::new(hook);
    match COMPLETION_HOOK.lock() {
        Ok(mut slot) => *slot = Some(hook),
        // Poison-tolerant like the rest of this best-effort signal path: a
        // panic elsewhere must not permanently disable completion wakeups.
        Err(poisoned) => *poisoned.into_inner() = Some(hook),
    }
}

/// Remove any installed hook, restoring the plain polled behaviour.
///
/// Hosts call this on shutdown so nothing they own is retained; tests call
/// it to reset this process-global slot between cases.
pub fn clear_completion_hook() {
    match COMPLETION_HOOK.lock() {
        Ok(mut slot) => *slot = None,
        Err(poisoned) => *poisoned.into_inner() = None,
    }
}

/// Fire the installed hook, if any. Called by [`JobCompleter`] once a job
/// has settled — never while its slot mutex is held.
pub(crate) fn notify_completion() {
    let hook = match COMPLETION_HOOK.lock() {
        Ok(slot) => slot.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Job;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The hook is a process-global, so these cases must not overlap each
    /// other — one test's `clear` would otherwise disarm another's hook
    /// mid-flight. Poison-tolerant: a panicking case must not wedge the
    /// rest of the file.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn serialised() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The hook fires for a job settled on another thread — the case the
    /// reactive host cannot discover on its own.
    #[test]
    fn a_worker_thread_settle_fires_the_hook() {
        let _guard = serialised();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        set_completion_hook(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });

        let (job, completer) = Job::<u32>::pending();
        let worker = std::thread::spawn(move || completer.succeed(7));
        worker.join().expect("worker finishes");

        assert!(job.poll().is_settled());
        assert!(
            hits.load(Ordering::SeqCst) >= 1,
            "settling a job must notify the host"
        );
        clear_completion_hook();
    }

    /// A dropped completer settles the job (as an error), so it owes the
    /// same wakeup — otherwise an abandoned worker hangs the UI forever.
    #[test]
    fn dropping_a_completer_fires_the_hook() {
        let _guard = serialised();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        set_completion_hook(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });

        let (job, completer) = Job::<u32>::pending();
        drop(completer);

        assert!(job.poll().is_settled());
        assert!(hits.load(Ordering::SeqCst) >= 1);
        clear_completion_hook();
    }

    /// Cleared means cleared: nothing is retained and nothing fires.
    #[test]
    fn a_cleared_hook_is_not_called() {
        let _guard = serialised();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        set_completion_hook(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        clear_completion_hook();

        let (_job, completer) = Job::<u32>::pending();
        completer.succeed(1);

        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }
}
