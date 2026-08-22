use std::{future::Future, time::Duration};

use futures::{future::LocalBoxFuture, task::SpawnError};
use lenso_kernel::{DeterministicDriver, DriverTask, LocalTask, RuntimeDriver, TaskOutcome};

/// Native test fallback for the browser Driver.
///
/// Browser builds use the JavaScript event loop implementation in `browser`.
/// Keeping this deterministic fallback makes the same public Driver behavior
/// testable on the native CI runner without requiring a browser process.
#[derive(Clone, Debug)]
pub struct BrowserDriver {
    inner: DeterministicDriver,
}

impl BrowserDriver {
    /// Creates a deterministic browser-host simulation at monotonic time zero.
    pub fn new() -> Self {
        Self {
            inner: DeterministicDriver::new(),
        }
    }

    /// Runs one root future on the local browser-host simulation.
    pub fn run<F: Future>(&self, future: F) -> F::Output {
        self.inner.run(future)
    }

    /// Schedules a root Kernel task on the local browser-host simulation.
    pub fn spawn_root(&self, task: LocalTask) -> Result<DriverTask, SpawnError> {
        self.spawn_local(task)
    }

    /// Pumps one local scheduler turn.
    pub fn pump(&self) {
        self.inner.run(self.inner.yield_now());
    }

    /// Advances the simulated browser monotonic clock and wakes expired timers.
    pub fn advance(&self, duration: Duration) {
        self.inner.advance(duration);
    }

    /// Requests shutdown from the simulated JavaScript host.
    pub fn request_shutdown(&self) {
        self.inner.request_shutdown();
    }

    /// Returns the simulated monotonic instant.
    pub fn now(&self) -> Duration {
        self.inner.now()
    }

    /// Returns the outcome of a Driver-owned task after it has been pumped.
    pub fn join(&self, task: DriverTask) -> TaskOutcome {
        self.run(task)
    }
}

impl Default for BrowserDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeDriver for BrowserDriver {
    fn now(&self) -> Duration {
        self.inner.now()
    }

    fn sleep_until(&self, deadline: Duration) -> LocalBoxFuture<'static, ()> {
        self.inner.sleep_until(deadline)
    }

    fn yield_now(&self) -> LocalBoxFuture<'static, ()> {
        self.inner.yield_now()
    }

    fn spawn_local(&self, task: LocalTask) -> Result<DriverTask, futures::task::SpawnError> {
        self.inner.spawn_local(task)
    }

    fn shutdown_requested(&self) -> bool {
        self.inner.shutdown_requested()
    }
}
