use std::{rc::Rc, sync::mpsc, time::Duration};

use futures::future::LocalBoxFuture;
use lenso_app_plan::{
    AppComposition, CapabilityBinding, CapabilityEndpointPlan, CapabilityRequirementPlan,
    ExecutionLaneId, ExecutionLanePlan, PluginInstancePlan,
};
use lenso_kernel::{
    ActivateContext, ExecutionAdapterCatalog, InvocationContext, PluginLifecycle, RuntimeFailure,
};
use lenso_runner::{
    CrossLaneRequestCatalog, LaneCancellationToken, LaneInvocationOptions, ReplicatedNativeApp,
    ReplicatedRunnerError,
};
use lenso_runtime_conformance::{
    ConformanceExecutionAdapter, ConformancePlugin, ConformancePluginFactory, PROBE_CAPABILITY_ID,
    PROBE_CONSUMER_PACKAGE_ID, PROBE_DESCRIPTOR_VERSION, PROBE_OPERATION,
    PROBE_PROVIDER_PACKAGE_ID, Probe, ProbeConsumerFactory, ProbeEndpoint, ProbeError,
    ProbeInvocationError, ProbeProvider, ProbeProviderFactory, ProbeRequest, ProbeResponse,
};

const EMPTY_PROBE_CONSUMER_PACKAGE_ID: &str = "fixture.empty-consumer";
const SLOW_PROBE_PROVIDER_PACKAGE_ID: &str = "fixture.slow-provider";

#[derive(Debug)]
struct EmptyConsumerFactory;

impl ConformancePluginFactory for EmptyConsumerFactory {
    fn package_id(&self) -> &'static str {
        EMPTY_PROBE_CONSUMER_PACKAGE_ID
    }

    fn instantiate(
        &self,
        _instance: &PluginInstancePlan,
    ) -> Result<ConformancePlugin, RuntimeFailure> {
        Ok(ConformancePlugin::default())
    }
}

#[derive(Debug)]
struct ReportingConsumerFactory {
    reported: mpsc::Sender<String>,
}

impl ConformancePluginFactory for ReportingConsumerFactory {
    fn package_id(&self) -> &'static str {
        PROBE_CONSUMER_PACKAGE_ID
    }

    fn instantiate(
        &self,
        _instance: &PluginInstancePlan,
    ) -> Result<ConformancePlugin, RuntimeFailure> {
        Ok(ConformancePlugin::with_lifecycle(
            Vec::new(),
            ReportingConsumerLifecycle {
                reported: self.reported.clone(),
            },
        ))
    }
}

#[derive(Debug)]
struct ReportingConsumerLifecycle {
    reported: mpsc::Sender<String>,
}

impl PluginLifecycle for ReportingConsumerLifecycle {
    fn activate(&self, context: ActivateContext) -> lenso_kernel::PluginFuture {
        let client =
            lenso_runtime_conformance::ProbeClient::from_dependencies(context.dependencies());
        let reported = self.reported.clone();
        Box::pin(async move {
            let response = client?
                .probe(ProbeRequest {
                    value: "module-client".to_owned(),
                })
                .await
                .map_err(|error| RuntimeFailure::PluginFailure {
                    detail: format!("typed cross-lane client failed: {error:?}"),
                })?;
            let _ = reported.send(response.value);
            Ok(())
        })
    }
}

#[derive(Debug)]
struct SlowProbeProviderFactory;

impl ConformancePluginFactory for SlowProbeProviderFactory {
    fn package_id(&self) -> &'static str {
        SLOW_PROBE_PROVIDER_PACKAGE_ID
    }

    fn instantiate(
        &self,
        _instance: &PluginInstancePlan,
    ) -> Result<ConformancePlugin, RuntimeFailure> {
        Ok(ConformancePlugin::new(vec![Rc::new(ProbeEndpoint::new(
            SlowProbeProvider,
        ))]))
    }
}

#[derive(Debug)]
struct SlowProbeProvider;

impl ProbeProvider for SlowProbeProvider {
    fn probe(
        &self,
        _context: InvocationContext,
        request: ProbeRequest,
    ) -> LocalBoxFuture<'static, Result<ProbeResponse, ProbeInvocationError>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok(ProbeResponse {
                value: format!("Echo: {}", request.value),
            })
        })
    }
}

fn two_lane_plan() -> lenso_app_plan::ResolvedAppPlan {
    AppComposition::new(
        vec![
            PluginInstancePlan::new("consumer", PROBE_CONSUMER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("frontend"))
                .with_requirement(CapabilityRequirementPlan::one(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                )),
            PluginInstancePlan::new("provider", PROBE_PROVIDER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("workers"))
                .with_capability(
                    CapabilityEndpointPlan::new(
                        PROBE_CAPABILITY_ID,
                        PROBE_DESCRIPTOR_VERSION,
                        [PROBE_OPERATION],
                    )
                    .with_cross_lane_transfer(),
                ),
        ],
        vec![CapabilityBinding::new(
            "consumer",
            PROBE_CAPABILITY_ID,
            PROBE_DESCRIPTOR_VERSION,
            "provider",
        )],
    )
    .with_execution_lanes(vec![
        ExecutionLanePlan::new("frontend"),
        ExecutionLanePlan::new("workers"),
    ])
    .resolve()
    .expect("two-lane fixture should resolve")
}

fn same_and_cross_lane_plan() -> lenso_app_plan::ResolvedAppPlan {
    AppComposition::new(
        vec![
            PluginInstancePlan::new("same-consumer", PROBE_CONSUMER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("frontend"))
                .with_requirement(CapabilityRequirementPlan::one(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                )),
            PluginInstancePlan::new("same-provider", PROBE_PROVIDER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("frontend"))
                .with_capability(
                    CapabilityEndpointPlan::new(
                        PROBE_CAPABILITY_ID,
                        PROBE_DESCRIPTOR_VERSION,
                        [PROBE_OPERATION],
                    )
                    .with_cross_lane_transfer(),
                ),
            PluginInstancePlan::new("cross-consumer", PROBE_CONSUMER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("frontend"))
                .with_requirement(CapabilityRequirementPlan::one(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                )),
            PluginInstancePlan::new("cross-provider", PROBE_PROVIDER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("workers"))
                .with_capability(
                    CapabilityEndpointPlan::new(
                        PROBE_CAPABILITY_ID,
                        PROBE_DESCRIPTOR_VERSION,
                        [PROBE_OPERATION],
                    )
                    .with_cross_lane_transfer(),
                ),
        ],
        vec![
            CapabilityBinding::new(
                "same-consumer",
                PROBE_CAPABILITY_ID,
                PROBE_DESCRIPTOR_VERSION,
                "same-provider",
            ),
            CapabilityBinding::new(
                "cross-consumer",
                PROBE_CAPABILITY_ID,
                PROBE_DESCRIPTOR_VERSION,
                "cross-provider",
            ),
        ],
    )
    .with_execution_lanes(vec![
        ExecutionLanePlan::new("frontend"),
        ExecutionLanePlan::new("workers"),
    ])
    .resolve()
    .expect("same/cross conformance fixture should resolve")
}

fn probe_adapters(lane: &ExecutionLaneId) -> ExecutionAdapterCatalog {
    let registry = match lane.as_str() {
        "frontend" => ConformanceExecutionAdapter::new()
            .with_factory(ProbeConsumerFactory)
            .with_factory(ProbeProviderFactory),
        "workers" => ConformanceExecutionAdapter::new().with_factory(ProbeProviderFactory),
        other => panic!("unexpected lane {other}"),
    };
    ExecutionAdapterCatalog::single(registry)
}

fn probe_transfers() -> CrossLaneRequestCatalog {
    CrossLaneRequestCatalog::new().with_request::<Probe>(&[PROBE_OPERATION])
}

fn slow_conformance_plan() -> lenso_app_plan::ResolvedAppPlan {
    AppComposition::new(
        vec![
            PluginInstancePlan::new("same-consumer", EMPTY_PROBE_CONSUMER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("frontend"))
                .with_requirement(CapabilityRequirementPlan::one(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                )),
            PluginInstancePlan::new("same-provider", SLOW_PROBE_PROVIDER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("frontend"))
                .with_capability(
                    CapabilityEndpointPlan::new(
                        PROBE_CAPABILITY_ID,
                        PROBE_DESCRIPTOR_VERSION,
                        [PROBE_OPERATION],
                    )
                    .with_limits(0, 1)
                    .with_cross_lane_transfer(),
                ),
            PluginInstancePlan::new("cross-consumer", EMPTY_PROBE_CONSUMER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("frontend"))
                .with_requirement(CapabilityRequirementPlan::one(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                )),
            PluginInstancePlan::new("cross-provider", SLOW_PROBE_PROVIDER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("workers"))
                .with_capability(
                    CapabilityEndpointPlan::new(
                        PROBE_CAPABILITY_ID,
                        PROBE_DESCRIPTOR_VERSION,
                        [PROBE_OPERATION],
                    )
                    .with_limits(0, 1)
                    .with_cross_lane_transfer(),
                ),
        ],
        vec![
            CapabilityBinding::new(
                "same-consumer",
                PROBE_CAPABILITY_ID,
                PROBE_DESCRIPTOR_VERSION,
                "same-provider",
            ),
            CapabilityBinding::new(
                "cross-consumer",
                PROBE_CAPABILITY_ID,
                PROBE_DESCRIPTOR_VERSION,
                "cross-provider",
            ),
        ],
    )
    .with_execution_lanes(vec![
        ExecutionLanePlan::new("frontend"),
        ExecutionLanePlan::new("workers"),
    ])
    .resolve()
    .expect("slow conformance fixture should resolve")
}

fn slow_adapters(lane: &ExecutionLaneId) -> ExecutionAdapterCatalog {
    let registry = match lane.as_str() {
        "frontend" => ConformanceExecutionAdapter::new()
            .with_factory(EmptyConsumerFactory)
            .with_factory(SlowProbeProviderFactory),
        "workers" => ConformanceExecutionAdapter::new().with_factory(SlowProbeProviderFactory),
        other => panic!("unexpected lane {other}"),
    };
    ExecutionAdapterCatalog::single(registry)
}

#[tokio::test(flavor = "current_thread")]
async fn one_plan_runs_two_kernel_lanes_and_invokes_across_them() {
    let (reported, report) = mpsc::channel();
    let app = ReplicatedNativeApp::start_with_transfers(
        two_lane_plan(),
        move |lane| {
            let registry = match lane.as_str() {
                "frontend" => {
                    ConformanceExecutionAdapter::new().with_factory(ReportingConsumerFactory {
                        reported: reported.clone(),
                    })
                }
                "workers" => ConformanceExecutionAdapter::new().with_factory(ProbeProviderFactory),
                other => panic!("unexpected lane {other}"),
            };
            ExecutionAdapterCatalog::single(registry)
        },
        probe_transfers(),
    )
    .expect("both Kernel lanes should start");

    assert_eq!(app.lane_count(), 2);
    assert_eq!(
        report
            .recv_timeout(Duration::from_secs(1))
            .expect("the Plugin's typed client should invoke across lanes"),
        "Echo: module-client"
    );
    let response = app
        .invoke::<Probe>(
            "consumer",
            PROBE_OPERATION,
            ProbeRequest {
                value: "Ada".to_owned(),
            },
        )
        .await
        .expect("cross-lane invocation should reach the provider")
        .expect("provider should return success");
    assert_eq!(response.value, "Echo: Ada");

    app.shutdown(Duration::from_secs(1))
        .await
        .expect("both lanes should stop cleanly");
}

#[test]
fn cross_lane_request_requires_a_registered_send_transfer() {
    let failure = ReplicatedNativeApp::start(two_lane_plan(), |_| {
        panic!("adapter creation must not run before transfer validation")
    })
    .expect_err("cross-lane request types must be registered");
    assert_eq!(
        failure,
        ReplicatedRunnerError::MissingCrossLaneRequestTransfer {
            capability: PROBE_CAPABILITY_ID.to_owned(),
        }
    );
}

#[tokio::test(flavor = "current_thread")]
async fn domain_errors_are_identical_on_same_and_cross_lane_bindings() {
    let app = ReplicatedNativeApp::start_with_transfers(
        same_and_cross_lane_plan(),
        probe_adapters,
        probe_transfers(),
    )
    .expect("both Kernel lanes should start");

    for caller in ["same-consumer", "cross-consumer"] {
        let outcome = app
            .invoke::<Probe>(
                caller,
                PROBE_OPERATION,
                ProbeRequest {
                    value: String::new(),
                },
            )
            .await
            .expect("transport should preserve a Domain Error");
        assert_eq!(outcome, Err(ProbeError::EmptyValue));
    }

    app.shutdown(Duration::from_secs(1))
        .await
        .expect("both lanes should stop cleanly");
}

#[tokio::test(flavor = "current_thread")]
async fn deadlines_are_identical_on_same_and_cross_lane_bindings() {
    let app = ReplicatedNativeApp::start_with_transfers(
        slow_conformance_plan(),
        slow_adapters,
        probe_transfers(),
    )
    .expect("both Kernel lanes should start");

    for caller in ["same-consumer", "cross-consumer"] {
        let failure = app
            .invoke_with_options::<Probe>(
                caller,
                PROBE_OPERATION,
                ProbeRequest {
                    value: "Ada".to_owned(),
                },
                LaneInvocationOptions::new().with_timeout(Duration::from_millis(1)),
            )
            .await
            .expect_err("the slow provider should exceed the deadline");
        assert!(matches!(failure, RuntimeFailure::DeadlineExceeded { .. }));
    }

    app.shutdown(Duration::from_secs(1))
        .await
        .expect("both lanes should stop cleanly");
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_is_identical_on_same_and_cross_lane_bindings() {
    let app = ReplicatedNativeApp::start_with_transfers(
        slow_conformance_plan(),
        slow_adapters,
        probe_transfers(),
    )
    .expect("both Kernel lanes should start");

    for caller in ["same-consumer", "cross-consumer"] {
        let cancellation = LaneCancellationToken::new();
        let cancel_after_dispatch = cancellation.clone();
        let invoke = app.invoke_with_options::<Probe>(
            caller,
            PROBE_OPERATION,
            ProbeRequest {
                value: "Ada".to_owned(),
            },
            LaneInvocationOptions::new().with_cancellation(cancellation),
        );
        let (failure, ()) = tokio::join!(
            async {
                invoke
                    .await
                    .expect_err("the provider should observe cooperative cancellation")
            },
            async move {
                tokio::time::sleep(Duration::from_millis(1)).await;
                cancel_after_dispatch.cancel();
            }
        );
        assert!(matches!(failure, RuntimeFailure::Cancelled { .. }));
    }

    app.shutdown(Duration::from_secs(1))
        .await
        .expect("both lanes should stop cleanly");
}

#[tokio::test(flavor = "current_thread")]
async fn resource_exhaustion_is_identical_on_same_and_cross_lane_bindings() {
    let app = ReplicatedNativeApp::start_with_transfers(
        slow_conformance_plan(),
        slow_adapters,
        probe_transfers(),
    )
    .expect("both Kernel lanes should start");

    for caller in ["same-consumer", "cross-consumer"] {
        let first = app.invoke::<Probe>(
            caller,
            PROBE_OPERATION,
            ProbeRequest {
                value: "first".to_owned(),
            },
        );
        let second = app.invoke::<Probe>(
            caller,
            PROBE_OPERATION,
            ProbeRequest {
                value: "second".to_owned(),
            },
        );
        let (first, second) = tokio::join!(first, second);
        let outcomes = [first, second];
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| {
                    matches!(outcome, Err(RuntimeFailure::ResourceExhausted { .. }))
                })
                .count(),
            1
        );
    }

    app.shutdown(Duration::from_secs(1))
        .await
        .expect("both lanes should stop cleanly");
}

#[tokio::test(flavor = "current_thread")]
async fn diagnostics_expose_lane_cpu_queue_depth_and_cross_lane_share() {
    let app = ReplicatedNativeApp::start_with_transfers(
        same_and_cross_lane_plan(),
        probe_adapters,
        probe_transfers(),
    )
    .expect("both Kernel lanes should start");

    tokio::time::sleep(Duration::from_millis(20)).await;
    let baseline = app.diagnostics_snapshot();

    for caller in ["same-consumer", "cross-consumer"] {
        app.invoke::<Probe>(
            caller,
            PROBE_OPERATION,
            ProbeRequest {
                value: "Ada".to_owned(),
            },
        )
        .await
        .expect("invocation transport should succeed")
        .expect("provider should succeed");
    }

    tokio::time::sleep(Duration::from_millis(20)).await;
    let diagnostics = app.diagnostics_snapshot();
    assert_eq!(diagnostics.lane_cpu_time().len(), 2);
    assert!(
        diagnostics
            .lane_cpu_time()
            .values()
            .all(|cpu_time| !cpu_time.is_zero())
    );
    assert_eq!(diagnostics.instance_queue_depth("same-provider"), Some(0));
    assert_eq!(diagnostics.instance_queue_depth("cross-provider"), Some(0));
    assert_eq!(diagnostics.total_messages() - baseline.total_messages(), 2);
    assert_eq!(
        diagnostics.cross_lane_messages() - baseline.cross_lane_messages(),
        1
    );

    app.shutdown(Duration::from_secs(1))
        .await
        .expect("both lanes should stop cleanly");
}
