use std::{collections::BTreeMap, time::Duration};

use lenso_app_plan::{ExecutionClassId, PluginCriticality, PluginInstancePlan, ResolvedAppPlan};
use lenso_kernel::{
    ActivateContext, DeactivateContext, DeterministicDriver, ExecutionAdapter,
    ExecutionAdapterCatalog, PluginFuture, PluginLifecycle, PreparedNativeApp,
    PreparedNativePlugin, RuntimeDriver, RuntimeFailure, TerminalOutcome,
};
use lenso_runner::run;

async fn configured_runtime_failure() {
    futures::future::ready(()).await;
    panic!("configured runtime failure");
}

#[derive(Debug)]
struct EmptyAdapter;

impl ExecutionAdapter for EmptyAdapter {
    fn execution_class(&self) -> ExecutionClassId {
        ExecutionClassId::native_rust()
    }

    fn prepare(&self, _plan: &ResolvedAppPlan) -> Result<PreparedNativeApp, RuntimeFailure> {
        Ok(PreparedNativeApp::empty())
    }
}

#[derive(Debug)]
struct FailingAdapter;

impl ExecutionAdapter for FailingAdapter {
    fn execution_class(&self) -> ExecutionClassId {
        ExecutionClassId::native_rust()
    }

    fn prepare(&self, _plan: &ResolvedAppPlan) -> Result<PreparedNativeApp, RuntimeFailure> {
        Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "startup failure".to_owned(),
        })
    }
}

#[derive(Clone, Copy, Debug)]
enum CleanupMode {
    Failure,
    Blocked,
}

#[derive(Debug)]
struct RuntimeFailureLifecycle;

impl PluginLifecycle for RuntimeFailureLifecycle {
    fn activate(&self, context: ActivateContext) -> PluginFuture {
        context
            .tasks()
            .spawn_local(Box::pin(configured_runtime_failure()))
            .expect("the failing managed task should be scheduled");
        Box::pin(async { Ok(()) })
    }
}

#[derive(Debug)]
struct CleanupLifecycle(CleanupMode);

impl PluginLifecycle for CleanupLifecycle {
    fn deactivate(&self, _context: DeactivateContext) -> PluginFuture {
        match self.0 {
            CleanupMode::Failure => Box::pin(async {
                Err(RuntimeFailure::Internal {
                    detail: "cleanup failed".to_owned(),
                })
            }),
            CleanupMode::Blocked => Box::pin(async {
                futures::future::pending::<()>().await;
                Ok(())
            }),
        }
    }
}

#[derive(Debug)]
struct RuntimeThenCleanupAdapter(CleanupMode);

impl ExecutionAdapter for RuntimeThenCleanupAdapter {
    fn execution_class(&self) -> ExecutionClassId {
        ExecutionClassId::native_rust()
    }

    fn prepare(&self, _plan: &ResolvedAppPlan) -> Result<PreparedNativeApp, RuntimeFailure> {
        Ok(PreparedNativeApp::new(
            Vec::new(),
            BTreeMap::from([
                (
                    "cleanup".to_owned(),
                    PreparedNativePlugin::new(Vec::new(), CleanupLifecycle(self.0)),
                ),
                (
                    "failing".to_owned(),
                    PreparedNativePlugin::new(Vec::new(), RuntimeFailureLifecycle),
                ),
            ]),
        ))
    }
}

fn terminal_failure_plan() -> ResolvedAppPlan {
    ResolvedAppPlan::new(
        vec![
            PluginInstancePlan::new("cleanup", "package.cleanup"),
            PluginInstancePlan::new("failing", "package.failing")
                .with_criticality(PluginCriticality::Critical),
        ],
        vec![],
    )
}

#[test]
fn runner_returns_clean_shutdown_after_the_driver_requests_stop() {
    let driver = DeterministicDriver::new();
    let shutdown_driver = driver.clone();
    driver
        .spawn_local(Box::pin(async move {
            shutdown_driver.request_shutdown();
        }))
        .expect("the deterministic Driver should accept the shutdown request");

    let outcome = driver.run(run(
        ResolvedAppPlan::empty(),
        driver.clone(),
        ExecutionAdapterCatalog::single(EmptyAdapter),
        Duration::from_secs(1),
    ));

    assert_eq!(outcome, Ok(TerminalOutcome::CleanShutdown));
}

#[test]
fn runner_returns_startup_failure_without_entering_shutdown() {
    let driver = DeterministicDriver::new();

    let outcome = driver.run(run(
        ResolvedAppPlan::new(vec![PluginInstancePlan::new("plugin", "package")], vec![]),
        driver.clone(),
        ExecutionAdapterCatalog::single(FailingAdapter),
        Duration::from_secs(1),
    ));

    assert!(matches!(
        outcome,
        Ok(TerminalOutcome::StartupFailure {
            error: RuntimeFailure::InvalidResolvedPlan { detail }
        }) if detail == "startup failure"
    ));
}

#[test]
fn runner_preserves_runtime_and_cleanup_failures() {
    let driver = DeterministicDriver::new();

    let outcome = driver.run(run(
        terminal_failure_plan(),
        driver.clone(),
        ExecutionAdapterCatalog::single(RuntimeThenCleanupAdapter(CleanupMode::Failure)),
        Duration::from_secs(1),
    ));

    assert!(matches!(
        outcome,
        Ok(TerminalOutcome::RuntimeFailureDuringShutdown {
            error: RuntimeFailure::PluginRestartExhausted { instance, attempts: 0 },
            cleanup_error: RuntimeFailure::Internal { detail },
        }) if instance == "failing" && detail == "cleanup failed"
    ));
}

#[test]
fn runner_preserves_runtime_failure_when_shutdown_times_out() {
    let driver = DeterministicDriver::new();
    let clock = driver.clone();
    driver
        .spawn_local(Box::pin(async move {
            for _ in 0..8 {
                clock.yield_now().await;
            }
            clock.advance(Duration::from_millis(10));
        }))
        .expect("the deadline clock should be scheduled");

    let outcome = driver.run(run(
        terminal_failure_plan(),
        driver.clone(),
        ExecutionAdapterCatalog::single(RuntimeThenCleanupAdapter(CleanupMode::Blocked)),
        Duration::from_millis(1),
    ));

    assert!(matches!(
        outcome,
        Ok(TerminalOutcome::RuntimeFailureWithShutdownTimeout {
            error: RuntimeFailure::PluginRestartExhausted { instance, attempts: 0 },
        }) if instance == "failing"
    ));
}
