//! Selected upstream request vectors, run on the real Workers Driver.
use crate::{EventGuard, driver::WorkersDriver, error};
use lenso_app_plan::{
    AppComposition, CapabilityBinding, CapabilityCardinality, CapabilityEndpointPlan,
    CapabilityRequirementPlan, PluginInstancePlan, ResolvedAppPlan,
};
use lenso_kernel::{Kernel, RuntimeFailure, ShutdownOutcome};
use lenso_runtime_conformance::*;
use std::time::Duration;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub async fn conformance_probe() -> Result<String, JsValue> {
    for (plan, expected) in [
        (
            ResolvedAppPlan::with_schema_version(0),
            "unsupported Plan schema version 0",
        ),
        (
            ResolvedAppPlan::new(
                vec![
                    PluginInstancePlan::new("provider", PROBE_PROVIDER_PACKAGE_ID).with_capability(
                        CapabilityEndpointPlan::new(
                            PROBE_CAPABILITY_ID,
                            PROBE_DESCRIPTOR_VERSION,
                            ["undeclared.operation"],
                        ),
                    ),
                ],
                vec![],
            ),
            "differs from its resolved Descriptor",
        ),
    ] {
        let driver = WorkersDriver::new();
        let _guard = EventGuard(driver.clone());
        let result = Kernel::start_native(
            plan,
            driver,
            ConformanceExecutionAdapter::new().with_factory(ProbeProviderFactory),
        )
        .await;
        if !matches!(result, Err(RuntimeFailure::InvalidResolvedPlan { ref detail }) if detail.contains(expected))
        {
            return Err(error(format!(
                "invalid plan accepted or error changed: {expected}"
            )));
        }
    }
    {
        let driver = WorkersDriver::new();
        let _guard = EventGuard(driver.clone());
        let app = Kernel::start_native(
            ResolvedAppPlan::new(vec![], vec![]),
            driver,
            ConformanceExecutionAdapter::new(),
        )
        .await
        .map_err(error)?;
        let unavailable = matches!(
            app.handle::<Probe>("consumer"),
            Err(RuntimeFailure::Unavailable {
                capability: PROBE_CAPABILITY_ID
            })
        );
        let shutdown = app.shutdown(Duration::from_secs(1)).await;
        if !unavailable || shutdown != ShutdownOutcome::Clean {
            return Err(error("missing binding outcome changed"));
        }
    }
    for (provider, prefix) in [
        (PROBE_PROVIDER_PACKAGE_ID, "Echo"),
        (ALTERNATE_PROBE_PROVIDER_PACKAGE_ID, "Alternate"),
    ] {
        // This is the upstream product-neutral conformance composition, not a
        // second product resolver or hand-authored deployment Plan.
        let plan = AppComposition::new(
            vec![
                PluginInstancePlan::new("provider", provider).with_capability(
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
        .map_err(error)?;
        let driver = WorkersDriver::new();
        let _guard = EventGuard(driver.clone());
        let adapter = ConformanceExecutionAdapter::new()
            .with_factory(ProbeProviderFactory)
            .with_factory(AlternateProbeProviderFactory)
            .with_factory(ProbeConsumerFactory);
        let app = Kernel::start_native(plan, driver, adapter)
            .await
            .map_err(error)?;
        let result = async {
            let client = ProbeClient::new(app.handle::<Probe>("consumer").map_err(error)?);
            let value = client
                .probe(ProbeRequest {
                    value: "Ada".into(),
                })
                .await
                .map_err(error)?;
            if value.value != format!("{prefix}: Ada") {
                return Err(error("typed response mismatch"));
            }
            if client
                .probe(ProbeRequest {
                    value: String::new(),
                })
                .await
                != Err(ProbeInvocationError::Domain(ProbeError::EmptyValue))
            {
                return Err(error("domain error changed"));
            }
            let unknown = app
                .invoke::<Probe>(
                    "consumer",
                    "missing.operation",
                    ProbeRequest {
                        value: "Ada".into(),
                    },
                )
                .await;
            if unknown
                != Err(RuntimeFailure::UnknownOperation {
                    capability: PROBE_CAPABILITY_ID,
                    operation: "missing.operation".into(),
                })
            {
                return Err(error("unknown operation changed"));
            }
            Ok(())
        }
        .await;
        let shutdown = app.shutdown(Duration::from_secs(1)).await;
        result?;
        if shutdown != ShutdownOutcome::Clean {
            return Err(error(shutdown));
        }
    }
    Ok(serde_json::json!({"conformance":"passed","providers":2,"vectors":["unsupported schema","operation table mismatch","missing binding","activation dependency","typed response","domain error","unknown operation","provider replacement","clean shutdown"]}).to_string())
}
