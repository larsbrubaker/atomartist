//! `FlakyProvider` — a deterministic fault injector wrapping another provider.
//!
//! The plan (§12) asks for "injectable latency, failures, stamp conflicts" as
//! the backbone of provider-agnostic tests: retry logic, the conflict dialog,
//! the offline queue, and the frame-loop job pump all need a backend that
//! misbehaves *predictably*.
//!
//! Determinism is the whole point, so there is no randomness and no clock
//! here. Failures are injected on a fixed call cadence (`fail_every`), and
//! latency is expressed in [`pump`](FlakyProvider::pump) ticks rather than
//! milliseconds — a test advances the simulated clock itself, exactly like
//! the frame-loop job pump will.

use std::sync::{Arc, Mutex, PoisonError};

use crate::error::StorageError;
use crate::job::Job;
use crate::provider::{
    Blob, Bytes, Capabilities, Entry, NativePicker, Precondition, Stamp, StorageProvider,
};
use crate::uri::StorageUri;

/// How the wrapper misbehaves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlakyConfig {
    /// Fail every Nth operation (1-based: `Some(3)` fails calls 3, 6, 9 …).
    /// `None` never injects a failure.
    pub fail_every: Option<usize>,
    /// Error produced by an injected failure.
    pub error: StorageError,
    /// Number of [`pump`](FlakyProvider::pump) ticks an operation stays
    /// pending before its result is delivered. `0` forwards the inner
    /// provider's job untouched, so a synchronous backend stays synchronous.
    pub latency_ticks: usize,
}

impl Default for FlakyConfig {
    fn default() -> Self {
        FlakyConfig {
            fail_every: None,
            error: StorageError::Io("injected failure".to_string()),
            latency_ticks: 0,
        }
    }
}

impl FlakyConfig {
    /// Fail every `n`th call with [`StorageError::Io`].
    pub fn failing_every(n: usize) -> Self {
        FlakyConfig {
            fail_every: Some(n),
            ..FlakyConfig::default()
        }
    }

    /// Delay every result by `ticks` pumps.
    pub fn with_latency(mut self, ticks: usize) -> Self {
        self.latency_ticks = ticks;
        self
    }

    /// Use a specific error for injected failures.
    pub fn with_error(mut self, error: StorageError) -> Self {
        self.error = error;
        self
    }
}

/// A deferred result: `remaining` pumps to wait, then `step` forwards the
/// inner job's outcome and reports whether it is done.
struct Deferred {
    remaining: usize,
    step: Box<dyn FnMut() -> bool + Send>,
}

#[derive(Default)]
struct FlakyState {
    calls: usize,
    deferred: Vec<Deferred>,
}

/// Wraps any [`StorageProvider`] and injects deterministic failures/latency.
pub struct FlakyProvider {
    inner: Arc<dyn StorageProvider>,
    config: FlakyConfig,
    state: Mutex<FlakyState>,
}

impl FlakyProvider {
    pub fn new(inner: Arc<dyn StorageProvider>, config: FlakyConfig) -> Self {
        FlakyProvider {
            inner,
            config,
            state: Mutex::new(FlakyState::default()),
        }
    }

    /// The provider being wrapped.
    pub fn inner(&self) -> &Arc<dyn StorageProvider> {
        &self.inner
    }

    /// How many operations have been issued so far — the quantity
    /// `fail_every` counts against.
    pub fn call_count(&self) -> usize {
        self.lock().calls
    }

    /// Advance the simulated clock by one tick, delivering any results whose
    /// latency has elapsed. Call this where the app would tick its frame
    /// loop. Returns the number of jobs still outstanding.
    pub fn pump(&self) -> usize {
        let mut state = self.lock();
        let mut still_pending = Vec::new();
        for mut deferred in std::mem::take(&mut state.deferred) {
            // Decrement first, so `latency_ticks: N` delivers on the Nth
            // pump rather than the (N+1)th.
            deferred.remaining = deferred.remaining.saturating_sub(1);
            if deferred.remaining > 0 || !(deferred.step)() {
                still_pending.push(deferred);
            }
        }
        state.deferred = still_pending;
        state.deferred.len()
    }

    /// Pump until every outstanding job has been delivered, or panic if that
    /// takes implausibly long (a hung inner provider).
    pub fn pump_until_idle(&self) {
        for _ in 0..10_000 {
            if self.pump() == 0 {
                return;
            }
        }
        panic!("FlakyProvider still had outstanding jobs after 10000 pumps");
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FlakyState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Count this call and report whether it should be failed outright.
    fn should_fail(&self) -> bool {
        let mut state = self.lock();
        state.calls += 1;
        match self.config.fail_every {
            Some(n) if n > 0 => state.calls % n == 0,
            _ => false,
        }
    }

    /// Apply the configured latency to `inner`'s job.
    fn delay<T: Send + 'static>(&self, inner: Job<T>) -> Job<T> {
        if self.config.latency_ticks == 0 {
            return inner;
        }
        let (job, completer) = Job::pending();
        let mut completer = Some(completer);
        let step = Box::new(
            move || match (inner.poll().is_settled(), completer.take()) {
                (true, Some(completer)) => {
                    match inner.take() {
                        Some(result) => completer.complete(result),
                        None => completer.fail(StorageError::Io(
                            "wrapped job produced no result".to_string(),
                        )),
                    }
                    true
                }
                (true, None) => true,
                (false, restored) => {
                    completer = restored;
                    false
                }
            },
        );
        self.lock().deferred.push(Deferred {
            remaining: self.config.latency_ticks,
            step,
        });
        job
    }

    /// Either inject the configured failure or run + delay the real call.
    fn run<T: Send + 'static>(&self, op: impl FnOnce() -> Job<T>) -> Job<T> {
        if self.should_fail() {
            return Job::failed(self.config.error.clone());
        }
        self.delay(op())
    }
}

impl StorageProvider for FlakyProvider {
    fn scheme(&self) -> &str {
        self.inner.scheme()
    }

    fn display_name(&self) -> &str {
        self.inner.display_name()
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn list(&self, dir: &StorageUri) -> Job<Vec<Entry>> {
        self.run(|| self.inner.list(dir))
    }

    fn read(&self, at: &StorageUri) -> Job<Blob> {
        self.run(|| self.inner.read(at))
    }

    fn write(&self, at: &StorageUri, bytes: Bytes, pre: Precondition) -> Job<Stamp> {
        self.run(|| self.inner.write(at, bytes, pre))
    }

    fn delete(&self, at: &StorageUri) -> Job<()> {
        self.run(|| self.inner.delete(at))
    }

    fn stat(&self, at: &StorageUri) -> Job<Option<Entry>> {
        self.run(|| self.inner.stat(at))
    }

    fn create_dir(&self, at: &StorageUri) -> Job<()> {
        self.run(|| self.inner.create_dir(at))
    }

    fn native_picker(&self) -> Option<&dyn NativePicker> {
        self.inner.native_picker()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobState;
    use crate::memory::MemoryProvider;

    fn wrapped(config: FlakyConfig) -> (FlakyProvider, StorageUri) {
        let inner = MemoryProvider::new("mem", "Memory");
        let root = inner.root();
        (FlakyProvider::new(Arc::new(inner), config), root)
    }

    #[test]
    fn fails_exactly_every_nth_call() {
        let (flaky, root) = wrapped(FlakyConfig::failing_every(3));
        let at = root.join("a.bin");

        let failed: Vec<bool> = (0..6)
            .map(|_| {
                flaky
                    .write(&at, b"x".to_vec(), Precondition::None)
                    .error()
                    .is_some()
            })
            .collect();
        assert_eq!(failed, vec![false, false, true, false, false, true]);
        assert_eq!(flaky.call_count(), 6);
        // The injected failure must not have reached the inner store.
        assert_eq!(
            flaky.read(&at).take(),
            Some(Ok(b"x".to_vec())),
            "successful writes still land in the wrapped provider"
        );
    }

    #[test]
    fn failure_schedule_is_deterministic() {
        let (flaky, root) = wrapped(FlakyConfig::failing_every(2).with_error(StorageError::Auth));
        let at = root.join("a.bin");
        let mut errors = Vec::new();
        for _ in 0..4 {
            errors.push(flaky.write(&at, b"x".to_vec(), Precondition::None).error());
        }
        assert_eq!(
            errors,
            vec![
                None,
                Some(StorageError::Auth),
                None,
                Some(StorageError::Auth)
            ]
        );
    }

    #[test]
    fn latency_holds_results_until_pumped() {
        let (flaky, root) = wrapped(FlakyConfig::default().with_latency(2));
        let at = root.join("a.bin");

        let write = flaky.write(&at, b"payload".to_vec(), Precondition::None);
        assert!(write.poll().is_pending(), "latency must defer the result");
        assert_eq!(flaky.pump(), 1);
        assert!(write.poll().is_pending(), "one pump is not enough");
        assert_eq!(
            flaky.pump(),
            0,
            "`latency_ticks: 2` delivers on the second pump"
        );
        assert_eq!(write.poll(), JobState::Ready);

        let read = flaky.read(&at);
        flaky.pump_until_idle();
        assert_eq!(read.take(), Some(Ok(b"payload".to_vec())));
    }

    #[test]
    fn zero_latency_forwards_the_inner_job_untouched() {
        let (flaky, root) = wrapped(FlakyConfig::default());
        let at = root.join("a.bin");
        assert_eq!(
            flaky.write(&at, b"x".to_vec(), Precondition::None).poll(),
            JobState::Ready
        );
    }
}
