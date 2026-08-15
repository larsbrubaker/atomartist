//! Waiting for a [`Job`] to settle, on both kinds of executor.
//!
//! The conformance suite (`crate::conformance`) is written once, as `async`
//! functions, and driven two ways:
//!
//! - **native** — [`block_on`] polls the future in a loop, yielding the
//!   thread between attempts. This is what `await_job` did before the suite
//!   became async, and it behaves identically for providers that complete
//!   inline or on a worker thread.
//! - **wasm** — the suite is awaited by `wasm-bindgen-test` on the browser
//!   event loop. Busy-polling cannot work there (spinning never lets the
//!   loop deliver a promise), so [`Settle`] registers a wake-up on the
//!   *macrotask* queue: a `setTimeout(0)`. A microtask (`Promise.resolve`)
//!   would not do — the browser drains newly queued microtasks before
//!   returning to the task queue, so a microtask-driven poll loop can starve
//!   the very tasks that resolve an OPFS promise.
//!
//! `Job` has no waker registry of its own (it is a mutex-protected slot the
//! frame loop polls once per tick), which is why waiting is expressed as
//! "poll, then arrange to be polled again" rather than as a subscription.
//!
//! ## Where the timeout lives
//!
//! The hang budget belongs to [`Settle`], not to [`block_on`]: one
//! conformance run awaits ~100 jobs, and a budget spent by the whole run
//! would let an early slow job starve a later one — and would report a
//! suite-wide timeout as though a single job hung. Each `settle()` call
//! therefore gets its own budget, measured as wall-clock time natively
//! (poll counts mean nothing on a loaded CI box) and as poll iterations on
//! wasm, where each iteration is one `setTimeout(0)` macrotask and no clock
//! is needed to bound it.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::error::StorageError;
use crate::job::Job;

/// How long one job may take to settle before it is declared hung. Long
/// enough for a real thread hand-off or a slow disk on a loaded machine,
/// short enough to fail a test rather than hang CI forever.
#[cfg(not(target_arch = "wasm32"))]
const JOB_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Per-job budget on wasm: each iteration is a `setTimeout(0)`, which the
/// browser clamps to ~4 ms, so this is tens of seconds of real time.
#[cfg(target_arch = "wasm32")]
const MAX_JOB_POLLS: usize = 10_000;

/// Backstop for [`block_on`] itself, so a future that is *not* made of
/// `settle()` calls cannot wedge a test run silently. Deliberately far
/// larger than [`JOB_TIMEOUT`]: the per-job budget is what should fire.
#[cfg(not(target_arch = "wasm32"))]
const BLOCK_ON_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Future that resolves when `job` settles, yielding its result.
pub struct Settle<'a, T> {
    job: &'a Job<T>,
    /// Budget for this one job, started on the first poll so that a future
    /// built early and awaited late is not charged for the wait.
    #[cfg(not(target_arch = "wasm32"))]
    deadline: Option<std::time::Instant>,
    #[cfg(target_arch = "wasm32")]
    polls: usize,
}

/// Await a job's result without assuming an executor.
pub fn settle<T>(job: &Job<T>) -> Settle<'_, T> {
    Settle {
        job,
        #[cfg(not(target_arch = "wasm32"))]
        deadline: None,
        #[cfg(target_arch = "wasm32")]
        polls: 0,
    }
}

impl<T> Future for Settle<'_, T> {
    type Output = Result<T, StorageError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // `Settle` holds only a shared reference and a counter, so it is
        // `Unpin` and may be reached through the pin.
        let this = self.get_mut();
        if this.job.poll().is_settled() {
            return match this.job.take() {
                Some(result) => Poll::Ready(result),
                None => panic!("job settled but produced no result (already taken?)"),
            };
        }
        this.charge_budget();
        schedule_wake(cx);
        Poll::Pending
    }
}

impl<T> Settle<'_, T> {
    /// Spend one unit of this job's budget, panicking when it runs out.
    #[cfg(not(target_arch = "wasm32"))]
    fn charge_budget(&mut self) {
        let deadline = *self
            .deadline
            .get_or_insert_with(|| std::time::Instant::now() + JOB_TIMEOUT);
        if std::time::Instant::now() >= deadline {
            panic!(
                "storage job did not settle within the {} s per-job timeout",
                JOB_TIMEOUT.as_secs()
            );
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn charge_budget(&mut self) {
        self.polls += 1;
        if self.polls >= MAX_JOB_POLLS {
            panic!(
                "storage job did not settle within the per-job budget of \
                 {MAX_JOB_POLLS} event-loop turns"
            );
        }
    }
}

/// Native: `block_on` re-polls on its own, so nothing has to be scheduled.
#[cfg(not(target_arch = "wasm32"))]
fn schedule_wake(_cx: &mut Context<'_>) {}

/// WASM: ask the host to poll us again on the next macrotask.
///
/// `setTimeout` is reached through the global object rather than through
/// `web_sys::window()` so this works unchanged in a worker, where there is
/// no `Window` but the same timer function exists. If the call fails (no
/// timers at all), the waker fires immediately: that degrades to a
/// microtask spin, which is worse but still makes progress.
#[cfg(target_arch = "wasm32")]
fn schedule_wake(cx: &mut Context<'_>) {
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::{JsCast, JsValue};

    let waker = cx.waker().clone();
    let callback = Closure::once_into_js(move || waker.wake());
    let scheduled = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("setTimeout"))
        .ok()
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
        .and_then(|set_timeout| {
            set_timeout
                .call2(&JsValue::NULL, &callback, &JsValue::from_f64(0.0))
                .ok()
        });
    if scheduled.is_none() {
        cx.waker().wake_by_ref();
    }
}

/// Run `future` to completion on the calling thread. Native only — a wasm
/// caller awaits the suite on the browser event loop instead.
///
/// The bound here covers the whole future (a conformance run is ~100 jobs);
/// an individual hung job is caught earlier, and named, by [`Settle`].
#[cfg(not(target_arch = "wasm32"))]
pub fn block_on<F: Future>(future: F) -> F::Output {
    use std::sync::Arc;
    use std::task::Wake;

    struct NoopWake;
    impl Wake for NoopWake {
        fn wake(self: Arc<Self>) {}
        fn wake_by_ref(self: &Arc<Self>) {}
    }

    let mut future = Box::pin(future);
    let waker = Arc::new(NoopWake).into();
    let mut cx = Context::from_waker(&waker);
    let deadline = std::time::Instant::now() + BLOCK_ON_TIMEOUT;
    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::yield_now(),
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "future passed to block_on did not complete within {} s",
                BLOCK_ON_TIMEOUT.as_secs()
            );
        }
    }
}

/// Poll `job` until it settles and return its result.
///
/// Public because provider authors writing their own native tests need the
/// same spin-free wait. Deliberately absent on wasm: blocking the browser's
/// only thread can never let a job complete — await [`settle`] instead.
#[cfg(not(target_arch = "wasm32"))]
pub fn await_job<T>(job: &Job<T>) -> Result<T, StorageError> {
    block_on(settle(job))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::job::spawn_blocking;

    /// A cancelled job settles as `Cancelled`, and `settle` reports that
    /// rather than waiting out its budget for a result that never comes.
    #[test]
    fn settle_on_a_cancelled_job_reports_cancelled() {
        let (job, _completer) = Job::<u32>::pending();
        job.cancel();
        assert_eq!(await_job(&job), Err(StorageError::Cancelled));
    }

    /// The native driver waits across a real thread hand-off: the job is
    /// still pending when `block_on` starts polling it.
    #[test]
    fn block_on_settles_a_spawn_blocking_job() {
        let job = spawn_blocking(|| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            Ok(21u32 * 2)
        });
        assert!(job.poll().is_pending(), "the worker must not have finished");
        assert_eq!(block_on(settle(&job)), Ok(42));
    }
}
