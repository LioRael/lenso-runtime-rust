//! Adapted from conformance 0.3.2 deterministic_schedules.rs; real Workers Driver scheduling.
use crate::{EventGuard, driver::WorkersDriver};
use std::{rc::Rc, time::Duration};
use wasm_bindgen::prelude::*;

use futures::future::LocalBoxFuture;
use lenso_app_plan::{
    AppComposition, CapabilityBinding, CapabilityEndpointPlan, CapabilityRequirementPlan,
    PluginInstancePlan,
};
use lenso_kernel::{
    CancellationToken, InvocationContext, Kernel, RuntimeDriver, RuntimeFailure, ShutdownOutcome,
};
use lenso_runtime_conformance::{
    ConformanceExecutionAdapter, ConformancePlugin, ConformancePluginFactory, PROBE_CAPABILITY_ID,
    PROBE_CONSUMER_PACKAGE_ID, PROBE_DESCRIPTOR_VERSION, PROBE_OPERATION, Probe,
    ProbeConsumerFactory, ProbeEndpoint, ProbeInvocationError, ProbeProvider, ProbeRequest,
    ProbeResponse,
};

const YIELDING_PROVIDER_PACKAGE_ID: &str = "lenso.runtime.conformance.yielding-probe-provider";

#[derive(Clone, Debug)]
struct YieldingProviderFactory {
    driver: WorkersDriver,
    yields: usize,
}

impl ConformancePluginFactory for YieldingProviderFactory {
    fn package_id(&self) -> &'static str {
        YIELDING_PROVIDER_PACKAGE_ID
    }

    fn instantiate(
        &self,
        _instance: &PluginInstancePlan,
    ) -> Result<ConformancePlugin, RuntimeFailure> {
        Ok(ConformancePlugin::new(vec![Rc::new(ProbeEndpoint::new(
            YieldingProvider {
                driver: self.driver.clone(),
                yields: self.yields,
            },
        ))]))
    }
}

#[derive(Clone, Debug)]
struct YieldingProvider {
    driver: WorkersDriver,
    yields: usize,
}

impl ProbeProvider for YieldingProvider {
    fn probe(
        &self,
        _context: InvocationContext,
        request: ProbeRequest,
    ) -> LocalBoxFuture<'static, Result<ProbeResponse, ProbeInvocationError>> {
        let driver = self.driver.clone();
        let yields = self.yields;
        Box::pin(async move {
            for _ in 0..yields {
                driver.yield_now().await;
            }
            Ok(ProbeResponse {
                value: request.value,
            })
        })
    }
}

fn plan() -> lenso_app_plan::ResolvedAppPlan {
    AppComposition::new(
        vec![
            PluginInstancePlan::new("consumer", PROBE_CONSUMER_PACKAGE_ID).with_requirement(
                CapabilityRequirementPlan::one(PROBE_CAPABILITY_ID, PROBE_DESCRIPTOR_VERSION),
            ),
            PluginInstancePlan::new("provider", YIELDING_PROVIDER_PACKAGE_ID).with_capability(
                CapabilityEndpointPlan::new(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                    [PROBE_OPERATION],
                )
                .with_limits(1, 1),
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
    .expect("the deterministic schedule Plan should resolve")
}

async fn cancellation_and_completion_interleavings_preserve_one_terminal_outcome() {
    for provider_yields in 0..=4 {
        for cancellation_yields in 0..=4 {
            let driver = WorkersDriver::new();
            let _guard = EventGuard(driver.clone());
            let adapter = ConformanceExecutionAdapter::new()
                .with_factory(ProbeConsumerFactory)
                .with_factory(YieldingProviderFactory {
                    driver: driver.clone(),
                    yields: provider_yields,
                });
            let app = (Kernel::start_native(plan(), driver.clone(), adapter))
                .await
                .expect("the deterministic schedule App should start");
            let handle = app
                .handle::<Probe>("consumer")
                .expect("the request binding should resolve");
            let cancellation = CancellationToken::new();
            let context = app.invocation_context(None, cancellation.clone());
            let invocation = handle.invoke_with_context(
                PROBE_OPERATION,
                context,
                ProbeRequest {
                    value: "value".to_owned(),
                },
            );
            let control_driver = driver.clone();
            let control = async move {
                for _ in 0..cancellation_yields {
                    control_driver.yield_now().await;
                }
                cancellation.cancel();
            };
            let (outcome, ()) = (futures::future::join(invocation, control)).await;
            assert!(
                matches!(outcome, Ok(Ok(ProbeResponse { ref value })) if value == "value")
                    || matches!(outcome, Err(RuntimeFailure::Cancelled { .. })),
                "unexpected outcome for provider_yields={provider_yields}, cancellation_yields={cancellation_yields}: {outcome:?}"
            );
            assert_eq!(
                (app.shutdown(Duration::from_secs(1))).await,
                ShutdownOutcome::Clean
            );
            assert!(matches!(
                (handle.invoke(
                    PROBE_OPERATION,
                    ProbeRequest {
                        value: "late".to_owned(),
                    },
                ))
                .await,
                Err(RuntimeFailure::AdmissionClosed)
            ));
        }
    }
}

async fn deadline_and_completion_interleavings_remain_deterministic() {
    for provider_yields in 0..=4 {
        let driver = WorkersDriver::new();
        let _guard = EventGuard(driver.clone());
        let adapter = ConformanceExecutionAdapter::new()
            .with_factory(ProbeConsumerFactory)
            .with_factory(YieldingProviderFactory {
                driver: driver.clone(),
                yields: provider_yields,
            });
        let app = (Kernel::start_native(plan(), driver.clone(), adapter))
            .await
            .expect("the deadline schedule App should start");
        let handle = app
            .handle::<Probe>("consumer")
            .expect("the request binding should resolve");
        let context =
            app.invocation_context_after(Duration::from_millis(1), CancellationToken::new());
        let invocation = handle.invoke_with_context(
            PROBE_OPERATION,
            context,
            ProbeRequest {
                value: "value".to_owned(),
            },
        );
        let clock = driver.clone();
        let advance = async move {
            clock.yield_now().await;
            clock
                .sleep_until(clock.now() + Duration::from_millis(1))
                .await;
        };
        let (outcome, ()) = (futures::future::join(invocation, advance)).await;
        assert!(
            matches!(outcome, Ok(Ok(ProbeResponse { ref value })) if value == "value")
                || matches!(outcome, Err(RuntimeFailure::DeadlineExceeded { .. })),
            "unexpected deadline outcome for provider_yields={provider_yields}: {outcome:?}"
        );
        assert_eq!(
            (app.shutdown(Duration::from_secs(1))).await,
            ShutdownOutcome::Clean
        );
    }
}

#[wasm_bindgen]
pub async fn schedules_probe() -> String {
    cancellation_and_completion_interleavings_preserve_one_terminal_outcome().await;
    deadline_and_completion_interleavings_remain_deterministic().await;
    serde_json::json!({"suite":"schedules","passed":true,"cases":["cancellation_and_completion_interleavings_preserve_one_terminal_outcome", "deadline_and_completion_interleavings_remain_deterministic"]}).to_string()
}
