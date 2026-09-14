//! Ported from lenso-runtime-conformance 0.3.2 tests/named_dependencies.rs.
//! Real Workers scheduling replaces deterministic run/advance; assertions fail the probe.
use crate::{EventGuard, driver::WorkersDriver};
use lenso_app_plan::{
    AppComposition, CapabilityBinding, CapabilityEndpointPlan, CapabilityRequirementPlan,
    PluginInstancePlan,
};
use lenso_kernel::{Kernel, RuntimeFailure};
use lenso_runtime_conformance::*;
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::prelude::*;

#[derive(Debug)]
struct Consumer;
impl ConformancePluginFactory for Consumer {
    fn package_id(&self) -> &'static str {
        "consumer"
    }
    fn instantiate(&self, _: &PluginInstancePlan) -> Result<ConformancePlugin, RuntimeFailure> {
        Ok(ConformancePlugin::new(vec![]))
    }
}

async fn generated_clients_use_named_views_without_changing_their_interface() {
    for destination in [None, Some("a"), Some("b")] {
        let consumer = PluginInstancePlan::new("consumer", "consumer")
            .with_authoring(2, "lenso.native-authoring@2")
            .with_requirement(
                CapabilityRequirementPlan::one(PROBE_CAPABILITY_ID, PROBE_DESCRIPTOR_VERSION)
                    .with_requirement_id("source"),
            )
            .with_requirement(
                CapabilityRequirementPlan::optional(PROBE_CAPABILITY_ID, PROBE_DESCRIPTOR_VERSION)
                    .with_requirement_id("destination"),
            );
        let provider = |id, package| {
            PluginInstancePlan::new(id, package).with_capability(CapabilityEndpointPlan::new(
                PROBE_CAPABILITY_ID,
                PROBE_DESCRIPTOR_VERSION,
                [PROBE_OPERATION],
            ))
        };
        let binding = |id, target| {
            CapabilityBinding::new(
                "consumer",
                PROBE_CAPABILITY_ID,
                PROBE_DESCRIPTOR_VERSION,
                target,
            )
            .with_requirement_id(id)
        };
        let mut bindings = vec![binding("source", "a")];
        if let Some(target) = destination {
            bindings.push(binding("destination", target));
        }
        let plan = AppComposition::new(
            vec![
                consumer,
                provider("a", PROBE_PROVIDER_PACKAGE_ID),
                provider("b", ALTERNATE_PROBE_PROVIDER_PACKAGE_ID),
            ],
            bindings,
        )
        .resolve()
        .unwrap();
        let driver = WorkersDriver::new();
        let _guard = EventGuard(driver.clone());
        let adapter = ConformanceExecutionAdapter::new()
            .with_factory(Consumer)
            .with_factory(ProbeProviderFactory)
            .with_factory(AlternateProbeProviderFactory);
        let app = (Kernel::start_native(plan, driver.clone(), adapter))
            .await
            .unwrap();
        let dependencies = app.dependencies("consumer").unwrap();
        assert!(matches!(
            dependencies.one::<Probe>(),
            Err(RuntimeFailure::AmbiguousBinding { providers: 2, .. })
        ));
        assert!(matches!(
            app.handle::<Probe>("consumer"),
            Err(RuntimeFailure::AmbiguousBinding { providers: 2, .. })
        ));
        let source = dependencies.requirement("source").unwrap();
        let source_client = ProbeClient::from_dependencies(&source).unwrap();
        assert_eq!(source.bindings()[0].provider_instance(), "a");
        assert!(
            (source_client.probe(ProbeRequest {
                value: "named".into()
            }))
            .await
            .is_ok()
        );
        let destination_view = dependencies.requirement("destination").unwrap();
        assert_eq!(destination_view.requirements().len(), 1);
        assert_eq!(
            destination_view.optional::<Probe>().unwrap().is_some(),
            destination.is_some()
        );
        if let Some(target) = destination {
            assert_eq!(destination_view.bindings()[0].provider_instance(), target);
        }
        assert_eq!(
            app.shutdown(std::time::Duration::from_secs(1)).await,
            lenso_kernel::ShutdownOutcome::Clean
        );
    }
}

#[derive(Debug)]
struct RetainingProvider(Rc<RefCell<Option<lenso_kernel::ExecutionLease>>>);
impl ProbeProvider for RetainingProvider {
    fn probe(
        &self,
        context: lenso_kernel::InvocationContext,
        request: ProbeRequest,
    ) -> futures::future::LocalBoxFuture<'static, Result<ProbeResponse, ProbeInvocationError>> {
        self.0.replace(Some(context.retain_execution().unwrap()));
        Box::pin(async move {
            Ok(ProbeResponse {
                value: request.value,
            })
        })
    }
}

#[derive(Debug)]
struct RetainingFactory(Rc<RefCell<Option<lenso_kernel::ExecutionLease>>>);
impl ConformancePluginFactory for RetainingFactory {
    fn package_id(&self) -> &'static str {
        "retaining"
    }
    fn instantiate(&self, _: &PluginInstancePlan) -> Result<ConformancePlugin, RuntimeFailure> {
        Ok(ConformancePlugin::new(vec![Rc::new(ProbeEndpoint::new(
            RetainingProvider(self.0.clone()),
        ))]))
    }
}

async fn two_named_handles_cannot_bypass_provider_capacity_after_a_terminal_reply() {
    let requirement = |id| {
        CapabilityRequirementPlan::one(PROBE_CAPABILITY_ID, PROBE_DESCRIPTOR_VERSION)
            .with_requirement_id(id)
    };
    let binding = |id| {
        CapabilityBinding::new(
            "consumer",
            PROBE_CAPABILITY_ID,
            PROBE_DESCRIPTOR_VERSION,
            "provider",
        )
        .with_requirement_id(id)
    };
    let plan = AppComposition::new(
        vec![
            PluginInstancePlan::new("consumer", "consumer")
                .with_authoring(2, "lenso.native-authoring@2")
                .with_requirement(requirement("source"))
                .with_requirement(requirement("destination")),
            PluginInstancePlan::new("provider", "retaining")
                .with_authoring(2, "lenso.native-authoring@2")
                .with_capability(
                    CapabilityEndpointPlan::new(
                        PROBE_CAPABILITY_ID,
                        PROBE_DESCRIPTOR_VERSION,
                        [PROBE_OPERATION],
                    )
                    .with_admission(lenso_app_plan::RequestAdmissionPlan::new(0, 1)),
                ),
        ],
        vec![binding("source"), binding("destination")],
    )
    .resolve()
    .unwrap();
    let lease = Rc::new(RefCell::new(None));
    let driver = WorkersDriver::new();
    let _guard = EventGuard(driver.clone());
    let app = (Kernel::start_native(
        plan,
        driver.clone(),
        ConformanceExecutionAdapter::new()
            .with_factory(Consumer)
            .with_factory(RetainingFactory(lease.clone())),
    ))
    .await
    .unwrap();
    let dependencies = app.dependencies("consumer").unwrap();
    let source = dependencies
        .requirement("source")
        .unwrap()
        .one::<Probe>()
        .unwrap();
    let destination = dependencies
        .requirement("destination")
        .unwrap()
        .one::<Probe>()
        .unwrap();
    let request = || ProbeRequest {
        value: "retained".into(),
    };
    assert!((source.invoke(PROBE_OPERATION, request())).await.is_ok());
    assert!(matches!(
        (destination.invoke(PROBE_OPERATION, request())).await,
        Err(RuntimeFailure::ResourceExhausted { .. })
    ));
    lease.borrow_mut().take().unwrap().settle();
    assert!(
        (destination.invoke(PROBE_OPERATION, request()))
            .await
            .is_ok()
    );
    // Losing the Adapter's proof is uncertainty, not execution completion.
    drop(lease.borrow_mut().take().unwrap());
    assert_eq!(
        (app.shutdown(std::time::Duration::ZERO)).await,
        lenso_kernel::ShutdownOutcome::Timeout
    );
}

#[wasm_bindgen]
pub async fn named_dependencies_probe() -> String {
    generated_clients_use_named_views_without_changing_their_interface().await;
    two_named_handles_cannot_bypass_provider_capacity_after_a_terminal_reply().await;
    serde_json::json!({"suite":"named_dependencies", "passed":true, "cases":["generated_clients_use_named_views_without_changing_their_interface", "two_named_handles_cannot_bypass_provider_capacity_after_a_terminal_reply"]}).to_string()
}
