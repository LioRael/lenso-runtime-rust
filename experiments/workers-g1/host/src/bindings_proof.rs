//! Remaining conformance 0.3.2 request vectors; upstream plans and assertions.
use crate::{EventGuard, driver::WorkersDriver};
use std::time::Duration;
use wasm_bindgen::prelude::*;

use lenso_app_plan::{
    AppComposition, CapabilityBinding, CapabilityCardinality, CapabilityEndpointPlan,
    CapabilityRequirementPlan, PlanResolutionError, PluginInstancePlan, ResolvedAppPlan,
};
use lenso_kernel::{Kernel, RuntimeFailure};
use lenso_runtime_conformance::ConformanceExecutionAdapter;
use lenso_runtime_conformance::{
    ALTERNATE_PROBE_PROVIDER_PACKAGE_ID, AlternateProbeProviderFactory, PROBE_CONSUMER_PACKAGE_ID,
    PROBE_PROVIDER_PACKAGE_ID, ProbeConsumerFactory, ProbeProviderFactory,
};
use lenso_runtime_conformance::{
    PROBE_CAPABILITY_ID, PROBE_DESCRIPTOR_VERSION, Probe, ProbeClient, ProbeInvocationError,
    ProbeRequest,
};

fn probe_composition(provider_package_id: &str) -> AppComposition {
    AppComposition::new(
        vec![
            PluginInstancePlan::new("provider", provider_package_id).with_capability(
                CapabilityEndpointPlan::new(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                    [lenso_runtime_conformance::PROBE_OPERATION],
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
}

fn probe_adapter() -> ConformanceExecutionAdapter {
    ConformanceExecutionAdapter::new()
        .with_factory(ProbeProviderFactory)
        .with_factory(AlternateProbeProviderFactory)
        .with_factory(ProbeConsumerFactory)
}

async fn composition_materializes_keyed_instances_requirements_and_deterministic_many_bindings() {
    let composition = AppComposition::new(
        vec![
            PluginInstancePlan::new("consumer", PROBE_CONSUMER_PACKAGE_ID).with_requirement(
                CapabilityRequirementPlan::new(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                    CapabilityCardinality::Many,
                ),
            ),
            PluginInstancePlan::new("provider-z", PROBE_PROVIDER_PACKAGE_ID).with_capability(
                CapabilityEndpointPlan::new(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                    [lenso_runtime_conformance::PROBE_OPERATION],
                ),
            ),
            PluginInstancePlan::new("provider-a", PROBE_PROVIDER_PACKAGE_ID).with_capability(
                CapabilityEndpointPlan::new(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                    [lenso_runtime_conformance::PROBE_OPERATION],
                ),
            ),
        ],
        vec![
            CapabilityBinding::new(
                "consumer",
                PROBE_CAPABILITY_ID,
                PROBE_DESCRIPTOR_VERSION,
                "provider-z",
            ),
            CapabilityBinding::new(
                "consumer",
                PROBE_CAPABILITY_ID,
                PROBE_DESCRIPTOR_VERSION,
                "provider-a",
            ),
        ],
    );

    let plan = composition.resolve().expect("many binding should resolve");

    assert_eq!(
        plan.plugin_instances()
            .iter()
            .map(PluginInstancePlan::instance_key)
            .collect::<Vec<_>>(),
        ["consumer", "provider-a", "provider-z"]
    );
    let consumer = &plan.plugin_instances()[0];
    assert_eq!(
        consumer.required_capabilities()[0].cardinality(),
        CapabilityCardinality::Many
    );
    assert_eq!(
        plan.capability_bindings()
            .iter()
            .map(CapabilityBinding::provider_instance)
            .collect::<Vec<_>>(),
        ["provider-a", "provider-z"]
    );
}

async fn missing_one_binding_is_rejected_before_native_boot() {
    let composition = AppComposition::new(
        vec![
            PluginInstancePlan::new("consumer", PROBE_CONSUMER_PACKAGE_ID).with_requirement(
                CapabilityRequirementPlan::new(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                    CapabilityCardinality::One,
                ),
            ),
        ],
        vec![],
    );

    assert_eq!(
        composition.resolve(),
        Err(PlanResolutionError::MissingOneBinding {
            consumer_instance: "consumer".to_owned(),
            capability_id: PROBE_CAPABILITY_ID.to_owned(),
        })
    );
}

async fn missing_one_binding_is_rejected_before_the_execution_adapter_runs() {
    let driver = WorkersDriver::new();
    let _driver_guard = EventGuard(driver.clone());
    let plan = ResolvedAppPlan::new(
        vec![
            PluginInstancePlan::new("consumer", PROBE_CONSUMER_PACKAGE_ID).with_requirement(
                CapabilityRequirementPlan::one(PROBE_CAPABILITY_ID, PROBE_DESCRIPTOR_VERSION),
            ),
        ],
        vec![],
    );

    let outcome =
        (Kernel::start_native(plan, driver.clone(), ConformanceExecutionAdapter::new())).await;

    assert!(matches!(
        outcome,
        Err(RuntimeFailure::InvalidResolvedPlan { detail })
            if detail.contains("missing one binding")
    ));
}

async fn optional_requirement_may_be_unbound() {
    let composition = AppComposition::new(
        vec![
            PluginInstancePlan::new("consumer", PROBE_CONSUMER_PACKAGE_ID).with_requirement(
                CapabilityRequirementPlan::optional(PROBE_CAPABILITY_ID, PROBE_DESCRIPTOR_VERSION),
            ),
        ],
        vec![],
    );

    let plan = composition
        .resolve()
        .expect("an absent optional binding should resolve");

    assert!(plan.capability_bindings().is_empty());
}

async fn many_requirement_may_be_unbound_and_fan_out_to_nothing() {
    let composition = AppComposition::new(
        vec![
            PluginInstancePlan::new("consumer", PROBE_CONSUMER_PACKAGE_ID).with_requirement(
                CapabilityRequirementPlan::many(PROBE_CAPABILITY_ID, PROBE_DESCRIPTOR_VERSION),
            ),
        ],
        vec![],
    );
    let plan = composition
        .resolve()
        .expect("an absent many binding should resolve");
    let driver = WorkersDriver::new();
    let _driver_guard = EventGuard(driver.clone());
    let app = (Kernel::start_native(
        plan,
        driver.clone(),
        ConformanceExecutionAdapter::new().with_factory(ProbeConsumerFactory),
    ))
    .await
    .expect("the consumer should start without many providers");

    let handle = app
        .many_handle::<lenso_runtime_conformance::Probe>("consumer")
        .expect("an empty many handle should be materialized");
    assert_eq!(handle.binding_count(), 0);
    let outcomes = (handle.invoke_many(
        lenso_runtime_conformance::PROBE_OPERATION,
        ProbeRequest {
            value: "Ada".to_owned(),
        },
    ))
    .await
    .expect("an empty many fan-out should succeed");
    assert!(outcomes.is_empty());

    assert_eq!(
        app.shutdown(Duration::from_secs(1)).await,
        lenso_kernel::ShutdownOutcome::Clean
    );
}

async fn a_singular_client_does_not_fallback_to_the_first_many_provider() {
    let composition = AppComposition::new(
        vec![
            PluginInstancePlan::new("consumer", PROBE_CONSUMER_PACKAGE_ID).with_requirement(
                CapabilityRequirementPlan::many(PROBE_CAPABILITY_ID, PROBE_DESCRIPTOR_VERSION),
            ),
            PluginInstancePlan::new("provider-z", PROBE_PROVIDER_PACKAGE_ID).with_capability(
                CapabilityEndpointPlan::new(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                    [lenso_runtime_conformance::PROBE_OPERATION],
                ),
            ),
            PluginInstancePlan::new("provider-a", PROBE_PROVIDER_PACKAGE_ID).with_capability(
                CapabilityEndpointPlan::new(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                    [lenso_runtime_conformance::PROBE_OPERATION],
                ),
            ),
        ],
        vec![
            CapabilityBinding::new(
                "consumer",
                PROBE_CAPABILITY_ID,
                PROBE_DESCRIPTOR_VERSION,
                "provider-z",
            ),
            CapabilityBinding::new(
                "consumer",
                PROBE_CAPABILITY_ID,
                PROBE_DESCRIPTOR_VERSION,
                "provider-a",
            ),
        ],
    );
    let plan = composition.resolve().expect("many binding should resolve");
    let registry = ConformanceExecutionAdapter::new()
        .with_factory(ProbeProviderFactory)
        .with_factory(ProbeConsumerFactory);
    let driver = WorkersDriver::new();
    let _driver_guard = EventGuard(driver.clone());
    let app = (Kernel::start_native(plan, driver.clone(), registry))
        .await
        .expect("the App should start with both providers");
    assert_eq!(
        app.binding_count::<lenso_runtime_conformance::Probe>("consumer"),
        2
    );
    let client = ProbeClient::new(
        app.handle::<Probe>("consumer")
            .expect("many binding should be present"),
    );

    let outcome = (client.probe(ProbeRequest {
        value: "Ada".to_owned(),
    }))
    .await;

    assert_eq!(
        outcome,
        Err(ProbeInvocationError::Runtime(
            RuntimeFailure::AmbiguousBinding {
                capability: PROBE_CAPABILITY_ID,
                providers: 2,
            },
        ))
    );

    let handle = app
        .many_handle::<lenso_runtime_conformance::Probe>("consumer")
        .expect("the many handle should be materialized");
    let outcomes = (handle.invoke_many(
        lenso_runtime_conformance::PROBE_OPERATION,
        ProbeRequest {
            value: "Ada".to_owned(),
        },
    ))
    .await
    .expect("both providers should receive the typed request");
    assert_eq!(
        outcomes
            .into_iter()
            .map(|outcome| outcome.unwrap().value)
            .collect::<Vec<_>>(),
        ["Echo: Ada", "Echo: Ada"]
    );

    assert_eq!(
        app.shutdown(Duration::from_secs(1)).await,
        lenso_kernel::ShutdownOutcome::Clean
    );
}

async fn ambiguous_one_binding_is_rejected() {
    let composition = probe_composition(PROBE_PROVIDER_PACKAGE_ID);
    let composition = AppComposition::new(
        composition.plugin_instances().to_vec(),
        vec![
            CapabilityBinding::new(
                "consumer",
                PROBE_CAPABILITY_ID,
                PROBE_DESCRIPTOR_VERSION,
                "provider",
            ),
            CapabilityBinding::new(
                "consumer",
                PROBE_CAPABILITY_ID,
                PROBE_DESCRIPTOR_VERSION,
                "provider",
            ),
        ],
    );

    assert_eq!(
        composition.resolve(),
        Err(PlanResolutionError::AmbiguousOneBinding {
            consumer_instance: "consumer".to_owned(),
            capability_id: PROBE_CAPABILITY_ID.to_owned(),
            providers: 2,
        })
    );
}

async fn required_one_bindings_cannot_form_an_activation_cycle() {
    let endpoint = || {
        CapabilityEndpointPlan::new(
            PROBE_CAPABILITY_ID,
            PROBE_DESCRIPTOR_VERSION,
            [lenso_runtime_conformance::PROBE_OPERATION],
        )
    };
    let requirement =
        || CapabilityRequirementPlan::one(PROBE_CAPABILITY_ID, PROBE_DESCRIPTOR_VERSION);
    let composition = AppComposition::new(
        vec![
            PluginInstancePlan::new("a", PROBE_PROVIDER_PACKAGE_ID)
                .with_capability(endpoint())
                .with_requirement(requirement()),
            PluginInstancePlan::new("b", PROBE_PROVIDER_PACKAGE_ID)
                .with_capability(endpoint())
                .with_requirement(requirement()),
        ],
        vec![
            CapabilityBinding::new("a", PROBE_CAPABILITY_ID, PROBE_DESCRIPTOR_VERSION, "b"),
            CapabilityBinding::new("b", PROBE_CAPABILITY_ID, PROBE_DESCRIPTOR_VERSION, "a"),
        ],
    );

    assert_eq!(
        composition.resolve(),
        Err(PlanResolutionError::ActivationCycle {
            instances: vec!["a".to_owned(), "b".to_owned()]
        })
    );
}

async fn invalid_provider_reference_is_rejected() {
    let composition = AppComposition::new(
        vec![
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
            "missing-provider",
        )],
    );

    assert_eq!(
        composition.resolve(),
        Err(PlanResolutionError::InvalidProviderReference {
            consumer_instance: "consumer".to_owned(),
            capability_id: PROBE_CAPABILITY_ID.to_owned(),
            provider_instance: "missing-provider".to_owned(),
        })
    );
}

async fn incompatible_capability_versions_are_rejected() {
    let composition = AppComposition::new(
        vec![
            PluginInstancePlan::new("consumer", PROBE_CONSUMER_PACKAGE_ID).with_requirement(
                CapabilityRequirementPlan::new(
                    PROBE_CAPABILITY_ID,
                    "2.0.0",
                    CapabilityCardinality::One,
                ),
            ),
            PluginInstancePlan::new("provider", PROBE_PROVIDER_PACKAGE_ID).with_capability(
                CapabilityEndpointPlan::new(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                    [lenso_runtime_conformance::PROBE_OPERATION],
                ),
            ),
        ],
        vec![CapabilityBinding::new(
            "consumer",
            PROBE_CAPABILITY_ID,
            "2.0.0",
            "provider",
        )],
    );

    assert_eq!(
        composition.resolve(),
        Err(PlanResolutionError::IncompatibleCapabilityVersion {
            consumer_instance: "consumer".to_owned(),
            capability_id: PROBE_CAPABILITY_ID.to_owned(),
            required: "2.0.0".to_owned(),
            provided: PROBE_DESCRIPTOR_VERSION.to_owned(),
            provider_instance: "provider".to_owned(),
        })
    );
}

async fn replacing_the_provider_changes_composition_and_plan_but_not_the_consumer_binding() {
    let first_plan = probe_composition(PROBE_PROVIDER_PACKAGE_ID)
        .resolve()
        .expect("the first provider should resolve");
    let second_plan = probe_composition(ALTERNATE_PROBE_PROVIDER_PACKAGE_ID)
        .resolve()
        .expect("the replacement provider should resolve");

    let first_driver = WorkersDriver::new();
    let _first_driver_guard = EventGuard(first_driver.clone());
    let first_app =
        (Kernel::start_native(first_plan.clone(), first_driver.clone(), probe_adapter()))
            .await
            .expect("the first selected provider should start");
    let second_driver = WorkersDriver::new();
    let _second_driver_guard = EventGuard(second_driver.clone());
    let second_app =
        (Kernel::start_native(second_plan.clone(), second_driver.clone(), probe_adapter()))
            .await
            .expect("the replacement provider should start");

    let first_response = (ProbeClient::new(
        first_app
            .handle::<Probe>("consumer")
            .expect("first binding should resolve"),
    )
    .probe(ProbeRequest {
        value: "Ada".to_owned(),
    }))
    .await;
    let second_response = (ProbeClient::new(
        second_app
            .handle::<Probe>("consumer")
            .expect("replacement binding should resolve"),
    )
    .probe(ProbeRequest {
        value: "Ada".to_owned(),
    }))
    .await;

    assert_eq!(first_response.unwrap().value, "Echo: Ada");
    assert_eq!(second_response.unwrap().value, "Alternate: Ada");
    assert_ne!(first_plan, second_plan);
    assert_eq!(
        first_plan
            .plugin_instances()
            .iter()
            .find(|instance| instance.instance_key() == "consumer"),
        second_plan
            .plugin_instances()
            .iter()
            .find(|instance| instance.instance_key() == "consumer")
    );
    assert_eq!(
        first_plan.capability_bindings(),
        second_plan.capability_bindings()
    );

    assert_eq!(
        first_app.shutdown(Duration::from_secs(1)).await,
        lenso_kernel::ShutdownOutcome::Clean
    );

    assert_eq!(
        second_app.shutdown(Duration::from_secs(1)).await,
        lenso_kernel::ShutdownOutcome::Clean
    );
}
#[wasm_bindgen]
pub async fn bindings_probe() -> String {
    composition_materializes_keyed_instances_requirements_and_deterministic_many_bindings().await;
    missing_one_binding_is_rejected_before_native_boot().await;
    missing_one_binding_is_rejected_before_the_execution_adapter_runs().await;
    optional_requirement_may_be_unbound().await;
    many_requirement_may_be_unbound_and_fan_out_to_nothing().await;
    a_singular_client_does_not_fallback_to_the_first_many_provider().await;
    ambiguous_one_binding_is_rejected().await;
    required_one_bindings_cannot_form_an_activation_cycle().await;
    invalid_provider_reference_is_rejected().await;
    incompatible_capability_versions_are_rejected().await;
    replacing_the_provider_changes_composition_and_plan_but_not_the_consumer_binding().await;
    serde_json::json!({"suite":"bindings","passed":true,"cases":["composition_materializes_keyed_instances_requirements_and_deterministic_many_bindings", "missing_one_binding_is_rejected_before_native_boot", "missing_one_binding_is_rejected_before_the_execution_adapter_runs", "optional_requirement_may_be_unbound", "many_requirement_may_be_unbound_and_fan_out_to_nothing", "a_singular_client_does_not_fallback_to_the_first_many_provider", "ambiguous_one_binding_is_rejected", "required_one_bindings_cannot_form_an_activation_cycle", "invalid_provider_reference_is_rejected", "incompatible_capability_versions_are_rejected", "replacing_the_provider_changes_composition_and_plan_but_not_the_consumer_binding"]}).to_string()
}
