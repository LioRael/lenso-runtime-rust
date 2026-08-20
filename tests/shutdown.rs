use std::{collections::BTreeMap, time::Duration};

use lenso_app_plan::ResolvedAppPlan;
use lenso_kernel::{
    DeterministicDriver, NativeExecutionAdapter, PreparedNativeApp, RuntimeDriver, RuntimeFailure,
    TerminalOutcome,
};
use lenso_runner::run_native;

#[derive(Debug)]
struct EmptyAdapter;

impl NativeExecutionAdapter for EmptyAdapter {
    fn prepare(&self, _plan: &ResolvedAppPlan) -> Result<PreparedNativeApp, RuntimeFailure> {
        Ok(PreparedNativeApp::with_modules(
            BTreeMap::new(),
            BTreeMap::new(),
        ))
    }
}

#[derive(Debug)]
struct FailingAdapter;

impl NativeExecutionAdapter for FailingAdapter {
    fn prepare(&self, _plan: &ResolvedAppPlan) -> Result<PreparedNativeApp, RuntimeFailure> {
        Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "startup failure".to_owned(),
        })
    }
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

    let outcome = driver.run(run_native(
        ResolvedAppPlan::empty(),
        driver.clone(),
        EmptyAdapter,
        Duration::from_secs(1),
    ));

    assert_eq!(outcome, Ok(TerminalOutcome::CleanShutdown));
}

#[test]
fn runner_returns_startup_failure_without_entering_shutdown() {
    let driver = DeterministicDriver::new();

    let outcome = driver.run(run_native(
        ResolvedAppPlan::empty(),
        driver.clone(),
        FailingAdapter,
        Duration::from_secs(1),
    ));

    assert!(matches!(
        outcome,
        Ok(TerminalOutcome::StartupFailure {
            error: RuntimeFailure::InvalidResolvedPlan { detail }
        }) if detail == "startup failure"
    ));
}
