//! Ported from lenso-runtime-conformance 0.3.2 tests/diagnostics.rs.
//! Real Workers scheduling replaces deterministic run/advance; assertions fail the probe.
use crate::{EventGuard, driver::WorkersDriver};
use wasm_bindgen::prelude::*;

use std::time::Duration;

use lenso_app_plan::{
    AppComposition, CapabilityBinding, CapabilityCardinality, CapabilityEndpointPlan,
    CapabilityRequirementPlan, PluginInstancePlan, ResolvedAppPlan,
};
use lenso_kernel::{
    DiagnosticEvent, DiagnosticFilter, DiagnosticOutcome, DiagnosticSource,
    DiagnosticSubscribeError, ExecutionAdapterCatalog, Kernel, RuntimeDiagnostics, RuntimeDriver,
    RuntimeFailureKind, ShutdownOutcome,
};
use lenso_runtime_conformance::ConformanceExecutionAdapter;
use lenso_runtime_conformance::{
    PROBE_CAPABILITY_ID, PROBE_DESCRIPTOR_VERSION, PROBE_OPERATION, Probe, ProbeRequest,
};
use lenso_runtime_conformance::{
    PROBE_CONSUMER_PACKAGE_ID, PROBE_PROVIDER_PACKAGE_ID, ProbeConsumerFactory,
    ProbeProviderFactory,
};

fn probe_plan() -> ResolvedAppPlan {
    AppComposition::new(
        vec![
            PluginInstancePlan::new("provider", PROBE_PROVIDER_PACKAGE_ID).with_capability(
                CapabilityEndpointPlan::new(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                    [PROBE_OPERATION],
                ),
            ),
            PluginInstancePlan::new("consumer", PROBE_CONSUMER_PACKAGE_ID).with_requirement(
                CapabilityRequirementPlan::new(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                    CapabilityCardinality::One,
                ),
            ),
        ],
        vec![CapabilityBinding::new(
            "consumer",
            PROBE_CAPABILITY_ID,
            PROBE_DESCRIPTOR_VERSION,
            "provider",
        )],
    )
    .resolve()
    .expect("the conformance Plan should resolve")
}

async fn diagnostics_filter_sources_and_drop_overflow_without_affecting_shutdown() {
    let driver = WorkersDriver::new();
    let _guard = EventGuard(driver.clone());
    let diagnostics = RuntimeDiagnostics::new();
    let lifecycle = diagnostics
        .subscribe(DiagnosticFilter::only(DiagnosticSource::Lifecycle), 1)
        .expect("a positive observer capacity should be accepted");
    let invocation = diagnostics
        .subscribe(DiagnosticFilter::only(DiagnosticSource::Invocation), 1)
        .expect("a positive observer capacity should be accepted");

    let app = (Kernel::start_with_diagnostics(
        ResolvedAppPlan::empty(),
        driver.clone(),
        ExecutionAdapterCatalog::new(),
        diagnostics,
    ))
    .await
    .expect("the empty App should start");

    assert!(app.is_ready());
    assert!(lifecycle.try_recv().is_some());
    assert!(lifecycle.dropped_count() > 0);
    assert!(invocation.try_recv().is_none());
    assert_eq!(
        (app.shutdown(Duration::from_secs(1))).await,
        ShutdownOutcome::Clean
    );
}

async fn observer_can_await_the_next_record() {
    let driver = WorkersDriver::new();
    let _guard = EventGuard(driver.clone());
    let diagnostics = RuntimeDiagnostics::new();
    let mut observer = diagnostics
        .subscribe_all(8)
        .expect("the diagnostics observer should be bounded");

    let start_driver = driver.clone();
    let start_diagnostics = diagnostics.clone();
    let (record, app) = (async {
        futures::future::join(observer.recv(), async move {
            start_driver.yield_now().await;
            Kernel::start_with_diagnostics(
                ResolvedAppPlan::empty(),
                start_driver.clone(),
                ExecutionAdapterCatalog::new(),
                start_diagnostics,
            )
            .await
            .expect("the empty App should start")
        })
        .await
    })
    .await;

    assert!(record.is_some());
    assert_eq!(
        (app.shutdown(Duration::from_secs(1))).await,
        ShutdownOutcome::Clean
    );
}

async fn zero_observers_do_not_change_empty_app_behavior() {
    let driver = WorkersDriver::new();
    let _guard = EventGuard(driver.clone());
    let diagnostics = RuntimeDiagnostics::new();

    let app = (Kernel::start_with_diagnostics(
        ResolvedAppPlan::empty(),
        driver.clone(),
        ExecutionAdapterCatalog::new(),
        diagnostics.clone(),
    ))
    .await
    .expect("the empty App should start without observers");

    assert_eq!(diagnostics.observer_count(), 0);
    assert_eq!(
        (app.shutdown(Duration::from_secs(1))).await,
        ShutdownOutcome::Clean
    );
}

async fn observer_disconnect_and_zero_capacity_are_non_fatal() {
    let diagnostics = RuntimeDiagnostics::new();
    assert!(matches!(
        diagnostics.subscribe_all(0),
        Err(DiagnosticSubscribeError::ZeroCapacity)
    ));

    let observer = diagnostics
        .subscribe_all(2)
        .expect("a positive observer capacity is required");
    assert_eq!(diagnostics.observer_count(), 1);
    drop(observer);
    assert_eq!(diagnostics.observer_count(), 0);

    let driver = WorkersDriver::new();
    let _guard = EventGuard(driver.clone());
    let app = (Kernel::start_with_diagnostics(
        ResolvedAppPlan::empty(),
        driver.clone(),
        ExecutionAdapterCatalog::new(),
        diagnostics,
    ))
    .await
    .expect("an observer disconnect must not affect App startup");

    assert!(app.is_ready());
    assert_eq!(
        (app.shutdown(Duration::from_secs(1))).await,
        ShutdownOutcome::Clean
    );
}

async fn shutdown_records_the_actual_admission_and_cleanup_boundaries() {
    let driver = WorkersDriver::new();
    let _guard = EventGuard(driver.clone());
    let diagnostics = RuntimeDiagnostics::new();
    let observer = diagnostics
        .subscribe_all(32)
        .expect("the diagnostics observer should be bounded");
    let app = (Kernel::start_with_diagnostics(
        ResolvedAppPlan::empty(),
        driver.clone(),
        ExecutionAdapterCatalog::new(),
        diagnostics,
    ))
    .await
    .expect("the empty App should start");

    app.request_shutdown();
    driver
        .sleep_until(driver.now() + Duration::from_millis(10))
        .await;
    assert_eq!(
        app.shutdown(Duration::from_secs(1)).await,
        ShutdownOutcome::Clean
    );
    let records = std::iter::from_fn(|| observer.try_recv()).collect::<Vec<_>>();
    let admission: Vec<_> = records
        .iter()
        .filter(|r| matches!(r.event, DiagnosticEvent::ShutdownAdmissionClosed))
        .collect();
    let cleanup: Vec<_> = records
        .iter()
        .filter(|r| matches!(r.event, DiagnosticEvent::ShutdownCleanupStarted { .. }))
        .collect();
    let completed: Vec<_> = records
        .iter()
        .filter(|r| matches!(r.event, DiagnosticEvent::ShutdownCompleted { .. }))
        .collect();
    assert_eq!(admission.len(), 1);
    assert_eq!(cleanup.len(), 1);
    assert_eq!(completed.len(), 1);
    assert!(cleanup[0].timestamp >= admission[0].timestamp + Duration::from_millis(10));
    assert!(completed[0].timestamp >= cleanup[0].timestamp);
    if let DiagnosticEvent::ShutdownCompleted { elapsed, .. } = completed[0].event {
        assert_eq!(elapsed, completed[0].timestamp - cleanup[0].timestamp);
    }
}

async fn diagnostics_do_not_treat_unresolved_caller_text_as_structural_identity() {
    let driver = WorkersDriver::new();
    let _guard = EventGuard(driver.clone());
    let diagnostics = RuntimeDiagnostics::new();
    let observer = diagnostics
        .subscribe_all(32)
        .expect("the diagnostics observer should be bounded");
    let app = (Kernel::start_with_diagnostics(
        ResolvedAppPlan::empty(),
        driver.clone(),
        ExecutionAdapterCatalog::new(),
        diagnostics,
    ))
    .await
    .expect("the empty App should start");

    let caller_text = "not-in-the-plan: secret-value";
    assert!(app.ensure_binding::<Probe>(caller_text).is_err());
    let result = (app
        .many_handle::<Probe>(caller_text)
        .expect("many requirements may have no providers")
        .invoke_many(
            PROBE_OPERATION,
            ProbeRequest {
                value: "Ada".to_owned(),
            },
        ))
    .await;
    assert!(matches!(result, Ok(ref responses) if responses.is_empty()));

    let records = std::iter::from_fn(|| observer.try_recv()).collect::<Vec<_>>();
    assert!(records.iter().any(|record| {
        matches!(
            record.event,
            DiagnosticEvent::RuntimeFailure { instance: None, .. }
        )
    }));
    assert!(records.iter().any(|record| {
        matches!(
            record.event,
            DiagnosticEvent::InvocationStarted {
                caller_instance: None,
                ..
            }
        )
    }));
    assert!(records.iter().all(|record| match &record.event {
        DiagnosticEvent::RuntimeFailure { instance, .. } =>
            instance.as_deref() != Some(caller_text),
        DiagnosticEvent::InvocationStarted {
            caller_instance, ..
        }
        | DiagnosticEvent::InvocationCompleted {
            caller_instance, ..
        } => caller_instance.as_deref() != Some(caller_text),
        _ => true,
    }));

    assert_eq!(
        (app.shutdown(Duration::from_secs(1))).await,
        ShutdownOutcome::Clean
    );
}

async fn request_diagnostics_expose_timing_and_failure_categories_without_domain_bodies() {
    let driver = WorkersDriver::new();
    let _guard = EventGuard(driver.clone());
    let diagnostics = RuntimeDiagnostics::new();
    let observer = diagnostics
        .subscribe_all(64)
        .expect("observer capacity is positive");
    let app = (Kernel::start_native_with_diagnostics(
        probe_plan(),
        driver.clone(),
        ConformanceExecutionAdapter::new()
            .with_factory(ProbeProviderFactory)
            .with_factory(ProbeConsumerFactory),
        diagnostics,
    ))
    .await
    .expect("the conformance App should start");

    let success_started = driver.now();
    let success = (app.invoke::<Probe>(
        "consumer",
        PROBE_OPERATION,
        ProbeRequest {
            value: "Ada".to_owned(),
        },
    ))
    .await;
    let success_finished = driver.now();
    assert!(success.is_ok());

    let domain_error = (app.invoke::<Probe>(
        "consumer",
        PROBE_OPERATION,
        ProbeRequest {
            value: String::new(),
        },
    ))
    .await;
    assert!(matches!(domain_error, Ok(Err(_))));

    let unknown = (app.invoke::<Probe>(
        "consumer",
        "unknown.operation",
        ProbeRequest {
            value: "Ada".to_owned(),
        },
    ))
    .await;
    assert!(unknown.is_err());

    let records = std::iter::from_fn(|| observer.try_recv()).collect::<Vec<_>>();
    assert!(records.iter().any(|record| {
        matches!(
            record.event,
            DiagnosticEvent::InvocationCompleted {
                outcome: DiagnosticOutcome::Succeeded,
                elapsed,
                ..
            } if elapsed <= success_finished - success_started
        )
    }));
    assert!(records.iter().any(|record| {
        matches!(
            record.event,
            DiagnosticEvent::InvocationCompleted {
                outcome: DiagnosticOutcome::DomainError,
                ..
            }
        )
    }));
    assert!(records.iter().any(|record| {
        matches!(
            record.event,
            DiagnosticEvent::RuntimeFailure {
                kind: RuntimeFailureKind::UnknownOperation,
                ..
            }
        )
    }));

    assert_eq!(
        (app.shutdown(Duration::from_secs(1))).await,
        ShutdownOutcome::Clean
    );
}

#[wasm_bindgen]
pub async fn diagnostics_probe() -> String {
    diagnostics_filter_sources_and_drop_overflow_without_affecting_shutdown().await;
    observer_can_await_the_next_record().await;
    zero_observers_do_not_change_empty_app_behavior().await;
    observer_disconnect_and_zero_capacity_are_non_fatal().await;
    shutdown_records_the_actual_admission_and_cleanup_boundaries().await;
    diagnostics_do_not_treat_unresolved_caller_text_as_structural_identity().await;
    request_diagnostics_expose_timing_and_failure_categories_without_domain_bodies().await;
    serde_json::json!({"suite":"diagnostics", "passed":true, "cases":["diagnostics_filter_sources_and_drop_overflow_without_affecting_shutdown", "observer_can_await_the_next_record", "zero_observers_do_not_change_empty_app_behavior", "observer_disconnect_and_zero_capacity_are_non_fatal", "shutdown_records_the_actual_admission_and_cleanup_boundaries", "diagnostics_do_not_treat_unresolved_caller_text_as_structural_identity", "request_diagnostics_expose_timing_and_failure_categories_without_domain_bodies"]}).to_string()
}
