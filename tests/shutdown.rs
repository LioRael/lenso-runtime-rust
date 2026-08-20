use std::{collections::BTreeMap, time::Duration};

use lenso_app_plan::{ExecutionClassId, ModuleInstancePlan, ResolvedAppPlan};
use lenso_kernel::{
    DeterministicDriver, ExecutionAdapter, ExecutionAdapterCatalog, PreparedNativeApp,
    RuntimeDriver, RuntimeFailure, TerminalOutcome,
};
use lenso_runner::run;

#[derive(Debug)]
struct EmptyAdapter;

impl ExecutionAdapter for EmptyAdapter {
    fn execution_class(&self) -> ExecutionClassId {
        ExecutionClassId::native_rust()
    }

    fn prepare(&self, _plan: &ResolvedAppPlan) -> Result<PreparedNativeApp, RuntimeFailure> {
        Ok(PreparedNativeApp::with_modules(Vec::new(), BTreeMap::new()))
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
        ResolvedAppPlan::new(vec![ModuleInstancePlan::new("module", "package")], vec![]),
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
