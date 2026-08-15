//! `Job<T>` — the async bridge between storage providers and the frame loop.
//!
//! Every fallible or slow provider call returns a `Job<T>`: a shared slot the
//! UI polls once per frame. Local providers fill the slot immediately
//! (`Job::ready`), so the desktop path keeps its zero-latency feel; network
//! providers hand a [`JobCompleter`] to a worker (a `std::thread` on native,
//! a `spawn_local` future on wasm) and the call site never learns the
//! difference.
//!
//! This is deliberately *not* `async fn` + a runtime: the trait stays
//! object-safe, no executor is forced on the native shell, and the frame tick
//! is already the natural place to apply results.
//!
//! ## Deviation from the architecture plan
//!
//! The plan sketched `poll(&self) -> JobState<&T>`. A reference cannot escape
//! the mutex guard that protects the slot, so the payload-carrying variant is
//! split in two: [`Job::poll`] reports state only, and the payload is reached
//! with [`Job::take`] (moves it out) or [`Job::with_ready`] (borrows it inside
//! a closure). Same information, no lifetime laundering.

use std::sync::{Arc, Mutex, PoisonError};

use crate::error::StorageError;

/// What a job is doing right now, as reported by [`Job::poll`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JobState {
    /// Still running. `progress` is `0.0..=1.0` when the provider can
    /// estimate it, `None` otherwise.
    Pending { progress: Option<f32> },
    /// Finished successfully; the payload is available from
    /// [`Job::take`] / [`Job::with_ready`].
    Ready,
    /// Finished with an error, available from [`Job::take`] /
    /// [`Job::with_error`].
    Failed,
    /// The payload was already moved out by [`Job::take`].
    Taken,
}

impl JobState {
    pub fn is_pending(self) -> bool {
        matches!(self, JobState::Pending { .. })
    }

    /// True once the job will never change state again.
    pub fn is_settled(self) -> bool {
        !self.is_pending()
    }
}

/// Interior state of a job. Providers never touch this directly; they go
/// through [`JobCompleter`].
enum Outcome<T> {
    Pending { progress: Option<f32> },
    Ready(T),
    Failed(StorageError),
    Taken,
}

struct JobSlot<T> {
    outcome: Outcome<T>,
    cancel_requested: bool,
}

/// Handle to storage work in flight.
pub struct Job<T> {
    slot: Arc<Mutex<JobSlot<T>>>,
}

impl<T> Clone for Job<T> {
    fn clone(&self) -> Self {
        Job {
            slot: Arc::clone(&self.slot),
        }
    }
}

impl<T> std::fmt::Debug for Job<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job").field("state", &self.poll()).finish()
    }
}

/// The write end of a pending job, handed to whatever performs the work.
///
/// Dropping a completer without resolving it fails the job with
/// [`StorageError::Io`] rather than leaving the UI polling forever.
pub struct JobCompleter<T> {
    slot: Arc<Mutex<JobSlot<T>>>,
}

impl<T> Job<T> {
    /// A job that is already finished — the synchronous provider path.
    pub fn ready(value: T) -> Job<T> {
        Job {
            slot: Arc::new(Mutex::new(JobSlot {
                outcome: Outcome::Ready(value),
                cancel_requested: false,
            })),
        }
    }

    /// A job that already failed.
    pub fn failed(err: StorageError) -> Job<T> {
        Job {
            slot: Arc::new(Mutex::new(JobSlot {
                outcome: Outcome::Failed(err),
                cancel_requested: false,
            })),
        }
    }

    /// A job whose result arrives later. The caller keeps the `Job`, the
    /// worker keeps the [`JobCompleter`].
    pub fn pending() -> (Job<T>, JobCompleter<T>) {
        let slot = Arc::new(Mutex::new(JobSlot {
            outcome: Outcome::Pending { progress: None },
            cancel_requested: false,
        }));
        (
            Job {
                slot: Arc::clone(&slot),
            },
            JobCompleter { slot },
        )
    }

    /// Shorthand for `Job::ready` / `Job::failed` from a `Result`.
    pub fn from_result(result: Result<T, StorageError>) -> Job<T> {
        match result {
            Ok(value) => Job::ready(value),
            Err(err) => Job::failed(err),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, JobSlot<T>> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Current state. Cheap enough to call every frame for every open job.
    pub fn poll(&self) -> JobState {
        match self.lock().outcome {
            Outcome::Pending { progress } => JobState::Pending { progress },
            Outcome::Ready(_) => JobState::Ready,
            Outcome::Failed(_) => JobState::Failed,
            Outcome::Taken => JobState::Taken,
        }
    }

    /// Move the result out, leaving the job in [`JobState::Taken`]. Returns
    /// `None` while pending or after a previous take.
    pub fn take(&self) -> Option<Result<T, StorageError>> {
        let mut slot = self.lock();
        match std::mem::replace(&mut slot.outcome, Outcome::Taken) {
            Outcome::Ready(value) => Some(Ok(value)),
            Outcome::Failed(err) => Some(Err(err)),
            other => {
                slot.outcome = other;
                None
            }
        }
    }

    /// Inspect a successful payload in place, without consuming it.
    ///
    /// **The closure runs while this job's mutex is held.** Do not call any
    /// method on the *same* job from inside it — `poll`, `take`, `error`,
    /// `cancel`, or formatting it with `{:?}` will deadlock. Copy out what
    /// you need and do the rest afterwards.
    pub fn with_ready<R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
        match &self.lock().outcome {
            Outcome::Ready(value) => Some(f(value)),
            _ => None,
        }
    }

    /// Clone of the failure, if the job failed.
    pub fn error(&self) -> Option<StorageError> {
        match &self.lock().outcome {
            Outcome::Failed(err) => Some(err.clone()),
            _ => None,
        }
    }

    /// Ask the worker to stop. The job settles as
    /// [`StorageError::Cancelled`] immediately so the UI can move on;
    /// well-behaved workers also observe [`JobCompleter::is_cancelled`] and
    /// abandon their work.
    pub fn cancel(&self) {
        let mut slot = self.lock();
        slot.cancel_requested = true;
        if matches!(slot.outcome, Outcome::Pending { .. }) {
            slot.outcome = Outcome::Failed(StorageError::Cancelled);
        }
    }

    /// Whether [`cancel`](Self::cancel) was called on this job.
    pub fn is_cancelled(&self) -> bool {
        self.lock().cancel_requested
    }
}

impl<T> JobCompleter<T> {
    fn lock(&self) -> std::sync::MutexGuard<'_, JobSlot<T>> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Publish a successful result. Ignored if the job already settled
    /// (for example because it was cancelled).
    pub fn succeed(self, value: T) {
        self.settle(Outcome::Ready(value));
    }

    /// Publish a failure.
    pub fn fail(self, err: StorageError) {
        self.settle(Outcome::Failed(err));
    }

    /// Publish a `Result` in one call.
    pub fn complete(self, result: Result<T, StorageError>) {
        match result {
            Ok(value) => self.succeed(value),
            Err(err) => self.fail(err),
        }
    }

    /// Report fractional progress for the status bar.
    ///
    /// The value is clamped to `0.0..=1.0`, and `NaN` becomes `None`
    /// ("unknown"), so [`JobState`] never holds a value that breaks its
    /// `PartialEq` or renders as a nonsense progress bar.
    pub fn set_progress(&self, progress: Option<f32>) {
        let progress = match progress {
            Some(value) if value.is_nan() => None,
            Some(value) => Some(value.clamp(0.0, 1.0)),
            None => None,
        };
        let mut slot = self.lock();
        if matches!(slot.outcome, Outcome::Pending { .. }) {
            slot.outcome = Outcome::Pending { progress };
        }
    }

    /// True once the holder of the `Job` asked to cancel; long operations
    /// should check this between chunks and return early.
    pub fn is_cancelled(&self) -> bool {
        self.lock().cancel_requested
    }

    /// True once the job has an outcome — so anything this completer
    /// publishes from now on is ignored.
    ///
    /// The counterpart to [`is_cancelled`](Self::is_cancelled) for workers
    /// that own *user-visible* state rather than a computation:
    /// [`Job::cancel`] settles the job on the spot, and a UI holding the
    /// completer (AtomArtist's file-picker modal, which keeps its dialog up
    /// until the user answers) must be able to notice that nobody is
    /// waiting for that answer any more and take itself down. Asking
    /// "was cancel *requested*" is not the same question — a job can be
    /// settled without it, and the useful trigger is the outcome, not the
    /// request.
    pub fn is_settled(&self) -> bool {
        !matches!(self.lock().outcome, Outcome::Pending { .. })
    }

    /// Takes `self` by value so a job can only be settled once. The `Drop`
    /// guard below then sees a non-`Pending` outcome and stays quiet.
    fn settle(self, outcome: Outcome<T>) {
        let mut slot = self.lock();
        if matches!(slot.outcome, Outcome::Pending { .. }) {
            slot.outcome = outcome;
        }
        // Release the guard before `self` drops: the `Drop` impl re-locks.
        drop(slot);
    }
}

impl<T> Drop for JobCompleter<T> {
    fn drop(&mut self) {
        let mut slot = self.lock();
        if matches!(slot.outcome, Outcome::Pending { .. }) {
            slot.outcome = Outcome::Failed(StorageError::Io(
                "storage worker dropped without producing a result".to_string(),
            ));
        }
    }
}

/// Run `work` off the main thread and resolve the returned job with its
/// result. Native only — wasm has no threads.
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_blocking<T, F>(work: F) -> Job<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, StorageError> + Send + 'static,
{
    let (job, completer) = Job::pending();
    std::thread::spawn(move || {
        let result = work();
        completer.complete(result);
    });
    job
}

/// Drive `future` on the browser event loop and resolve the returned job
/// with its output. WASM only — the counterpart to [`spawn_blocking`].
#[cfg(target_arch = "wasm32")]
pub fn spawn_local<T, F>(future: F) -> Job<T>
where
    T: 'static,
    F: std::future::Future<Output = Result<T, StorageError>> + 'static,
{
    let (job, completer) = Job::pending();
    wasm_bindgen_futures::spawn_local(async move {
        let result = future.await;
        completer.complete(result);
    });
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_job_is_settled_immediately() {
        let job = Job::ready(7u32);
        assert_eq!(job.poll(), JobState::Ready);
        assert_eq!(job.with_ready(|v| *v), Some(7));
        assert_eq!(job.take(), Some(Ok(7)));
        assert_eq!(job.poll(), JobState::Taken);
        assert!(job.take().is_none());
    }

    #[test]
    fn failed_job_reports_its_error() {
        let job = Job::<u32>::failed(StorageError::NotFound);
        assert_eq!(job.poll(), JobState::Failed);
        assert_eq!(job.error(), Some(StorageError::NotFound));
        assert_eq!(job.take(), Some(Err(StorageError::NotFound)));
    }

    #[test]
    fn pending_job_settles_through_its_completer() {
        let (job, completer) = Job::pending();
        assert_eq!(job.poll(), JobState::Pending { progress: None });
        completer.set_progress(Some(0.5));
        assert_eq!(
            job.poll(),
            JobState::Pending {
                progress: Some(0.5)
            }
        );
        completer.succeed("done");
        assert_eq!(job.poll(), JobState::Ready);
        assert_eq!(job.take(), Some(Ok("done")));
    }

    #[test]
    fn progress_is_clamped_and_nan_reads_as_unknown() {
        let (job, completer) = Job::<u32>::pending();

        completer.set_progress(Some(2.5));
        assert_eq!(
            job.poll(),
            JobState::Pending {
                progress: Some(1.0)
            }
        );
        completer.set_progress(Some(-1.0));
        assert_eq!(
            job.poll(),
            JobState::Pending {
                progress: Some(0.0)
            }
        );

        completer.set_progress(Some(f32::NAN));
        let state = job.poll();
        assert_eq!(state, JobState::Pending { progress: None });
        // PartialEq stays reflexive, which a NaN payload would break.
        assert_eq!(state, state);
    }

    #[test]
    fn dropping_the_completer_fails_the_job() {
        let (job, completer) = Job::<u32>::pending();
        drop(completer);
        assert_eq!(job.poll(), JobState::Failed);
        assert!(matches!(job.error(), Some(StorageError::Io(_))));
    }

    #[test]
    fn cancel_settles_the_job_and_is_visible_to_the_worker() {
        let (job, completer) = Job::<u32>::pending();
        job.cancel();
        assert!(completer.is_cancelled());
        assert_eq!(job.error(), Some(StorageError::Cancelled));
        // A late result from the worker does not overwrite the cancellation.
        completer.succeed(3);
        assert_eq!(job.error(), Some(StorageError::Cancelled));
    }

    /// A completer can tell whether the job it holds still wants an
    /// answer — the question a UI that keeps a dialog up until the user
    /// replies has to ask, since a cancel settles the job behind its back.
    #[test]
    fn a_completer_sees_that_its_job_already_settled() {
        let (job, completer) = Job::<u32>::pending();
        assert!(!completer.is_settled(), "nothing has happened yet");

        job.cancel();
        assert!(completer.is_settled(), "a cancelled job has an outcome");
        // And publishing anyway is the no-op it always was.
        completer.succeed(1);
        assert_eq!(job.error(), Some(StorageError::Cancelled));

        // Progress is not an outcome.
        let (_job, completer) = Job::<u32>::pending();
        completer.set_progress(Some(0.5));
        assert!(!completer.is_settled());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn spawn_blocking_resolves_on_a_worker_thread() {
        let job = spawn_blocking(|| Ok(21u32 * 2));
        let mut result = None;
        for _ in 0..1_000_000 {
            if job.poll().is_settled() {
                result = job.take();
                break;
            }
            std::thread::yield_now();
        }
        assert_eq!(result, Some(Ok(42)));
    }
}
