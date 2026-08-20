//! Native Tokio Runtime Driver for the Lenso vNext Kernel.

use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

use futures::{
    channel::oneshot,
    future::{AbortHandle, Abortable},
};
use lenso_app_plan::{PlanResolutionError, ResolvedAppPlan};
use lenso_kernel::{
    DriverTask, LocalTask, NativeExecutionAdapter, PlanValidationError, RuntimeDriver,
    ShutdownOutcome, TaskOutcome, TerminalOutcome,
};

/// Tokio-backed Runtime Driver used by the native App Runner.
#[derive(Clone, Debug)]
pub struct TokioDriver {
    started_at: Instant,
    shutdown_requested: Rc<Cell<bool>>,
    jitter_state: Rc<Cell<u64>>,
}

impl TokioDriver {
    /// Creates a Driver bound to the current Tokio local task context.
    pub fn new() -> Self {
        let started_at = Instant::now();
        Self {
            started_at,
            shutdown_requested: Rc::new(Cell::new(false)),
            jitter_state: Rc::new(Cell::new(
                u64::try_from(started_at.elapsed().as_nanos()).unwrap_or(u64::MAX)
                    ^ 0x9e37_79b9_7f4a_7c15,
            )),
        }
    }

    /// Requests cooperative Kernel shutdown.
    pub fn request_shutdown(&self) {
        self.shutdown_requested.set(true);
    }
}

impl Default for TokioDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeDriver for TokioDriver {
    fn now(&self) -> Duration {
        self.started_at.elapsed()
    }

    fn sleep_until(&self, deadline: Duration) -> futures::future::LocalBoxFuture<'static, ()> {
        let target = self.started_at + deadline;
        Box::pin(async move {
            tokio::time::sleep_until(tokio::time::Instant::from_std(target)).await;
        })
    }

    fn yield_now(&self) -> futures::future::LocalBoxFuture<'static, ()> {
        Box::pin(tokio::task::yield_now())
    }

    fn jitter(&self, maximum: Duration) -> Duration {
        if maximum.is_zero() {
            return Duration::ZERO;
        }
        let next = self
            .jitter_state
            .get()
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.jitter_state.set(next);
        let maximum_nanos = maximum.as_nanos().min(u128::from(u64::MAX));
        let jitter_nanos = u128::from(next) % maximum_nanos.saturating_add(1);
        Duration::from_nanos(u64::try_from(jitter_nanos).unwrap_or(u64::MAX))
    }

    fn spawn_local(&self, task: LocalTask) -> Result<DriverTask, futures::task::SpawnError> {
        let (abort, registration) = AbortHandle::new_pair();
        let (completed, completion) = oneshot::channel();
        tokio::task::spawn_local(async move {
            let outcome = if Abortable::new(task, registration).await.is_ok() {
                TaskOutcome::Completed
            } else {
                TaskOutcome::Cancelled
            };
            let _ = completed.send(outcome);
        });
        Ok(DriverTask::new(abort, completion))
    }

    fn shutdown_requested(&self) -> bool {
        self.shutdown_requested.get()
    }
}

/// Runs a native App until the Runtime Driver requests shutdown or supervision fails.
pub async fn run_native<D: RuntimeDriver, A: NativeExecutionAdapter>(
    plan: ResolvedAppPlan,
    driver: D,
    adapter: A,
    shutdown_timeout: Duration,
) -> Result<TerminalOutcome, PlanValidationError> {
    if let Err(error) = plan.validate() {
        return Err(match error {
            PlanResolutionError::UnsupportedSchemaVersion { expected, actual } => {
                PlanValidationError::UnsupportedSchemaVersion { expected, actual }
            }
            error => PlanValidationError::InvalidResolvedPlan {
                detail: error.to_string(),
            },
        });
    }

    let app = match lenso_kernel::Kernel::start_native(plan, driver.clone(), adapter).await {
        Ok(app) => app,
        Err(error) => return Ok(TerminalOutcome::StartupFailure { error }),
    };
    while !driver.shutdown_requested() && !app.is_failed() {
        driver.yield_now().await;
    }
    if let Some(error) = app.terminal_failure() {
        let _ = app.shutdown(shutdown_timeout).await;
        return Ok(TerminalOutcome::RuntimeFailure { error });
    }
    Ok(match app.shutdown(shutdown_timeout).await {
        ShutdownOutcome::Clean => TerminalOutcome::CleanShutdown,
        ShutdownOutcome::RuntimeFailure { error } => TerminalOutcome::RuntimeFailure { error },
        ShutdownOutcome::Timeout => TerminalOutcome::ShutdownTimeout,
    })
}
