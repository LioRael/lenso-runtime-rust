use std::{rc::Rc, sync::mpsc, time::Duration};

use futures::future::LocalBoxFuture;
use lenso_app_plan::{
    AppComposition, CapabilityBinding, CapabilityEndpointPlan, CapabilityRequirementPlan,
    ExecutionLaneId, ExecutionLanePlan, ModuleInstancePlan,
};
use lenso_capability_greeting::{
    GREET_OPERATION, GreetError, GreetRequest, GreetResponse, Greeting, GreetingEndpoint,
    GreetingInvocationError, GreetingProvider,
};
use lenso_kernel::{
    ActivateContext, ExecutionAdapterCatalog, InvocationContext, ModuleLifecycle, RuntimeFailure,
};
use lenso_native_adapter::{
    NativeModuleFactory, NativeModuleFactoryContext, NativeModuleInstance, NativeModuleRegistry,
};
use lenso_native_greeter::{
    CONSUMER_PACKAGE_ID, ConsumerFactory, GREETER_PACKAGE_ID, GreeterFactory,
};
use lenso_runner::{
    CrossLaneRequestCatalog, LaneCancellationToken, LaneInvocationOptions, ReplicatedNativeApp,
    ReplicatedRunnerError,
};

const EMPTY_CONSUMER_PACKAGE_ID: &str = "fixture.empty-consumer";
const SLOW_GREETER_PACKAGE_ID: &str = "fixture.slow-greeter";

#[derive(Debug)]
struct EmptyConsumerFactory;

impl NativeModuleFactory for EmptyConsumerFactory {
    fn package_id(&self) -> &'static str {
        EMPTY_CONSUMER_PACKAGE_ID
    }

    fn instantiate(
        &self,
        _context: NativeModuleFactoryContext<'_>,
    ) -> Result<NativeModuleInstance, RuntimeFailure> {
        Ok(NativeModuleInstance::default())
    }
}

#[derive(Debug)]
struct ReportingConsumerFactory {
    reported: mpsc::Sender<String>,
}

impl NativeModuleFactory for ReportingConsumerFactory {
    fn package_id(&self) -> &'static str {
        CONSUMER_PACKAGE_ID
    }

    fn instantiate(
        &self,
        _context: NativeModuleFactoryContext<'_>,
    ) -> Result<NativeModuleInstance, RuntimeFailure> {
        Ok(NativeModuleInstance::with_lifecycle(
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

impl ModuleLifecycle for ReportingConsumerLifecycle {
    fn activate(&self, context: ActivateContext) -> lenso_kernel::ModuleFuture {
        let client =
            lenso_capability_greeting::GreetingClient::from_dependencies(context.dependencies());
        let reported = self.reported.clone();
        Box::pin(async move {
            let response = client?
                .greet(GreetRequest {
                    name: "module-client".to_owned(),
                })
                .await
                .map_err(|error| RuntimeFailure::ModuleFailure {
                    detail: format!("generated cross-lane client failed: {error:?}"),
                })?;
            let _ = reported.send(response.message);
            Ok(())
        })
    }
}

#[derive(Debug)]
struct SlowGreeterFactory;

impl NativeModuleFactory for SlowGreeterFactory {
    fn package_id(&self) -> &'static str {
        SLOW_GREETER_PACKAGE_ID
    }

    fn instantiate(
        &self,
        _context: NativeModuleFactoryContext<'_>,
    ) -> Result<NativeModuleInstance, RuntimeFailure> {
        Ok(NativeModuleInstance::new(vec![Rc::new(
            GreetingEndpoint::new(SlowGreeter),
        )]))
    }
}

#[derive(Debug)]
struct SlowGreeter;

impl GreetingProvider for SlowGreeter {
    fn greet(
        &self,
        _context: InvocationContext,
        request: GreetRequest,
    ) -> LocalBoxFuture<'static, Result<GreetResponse, GreetingInvocationError>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok(GreetResponse {
                message: format!("Hello, {}!", request.name),
            })
        })
    }
}

fn two_lane_plan() -> lenso_app_plan::ResolvedAppPlan {
    AppComposition::new(
        vec![
            ModuleInstancePlan::new("consumer", CONSUMER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("frontend"))
                .with_requirement(CapabilityRequirementPlan::one(
                    "example.greeting@1",
                    "1.0.0",
                )),
            ModuleInstancePlan::new("provider", GREETER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("workers"))
                .with_capability(
                    CapabilityEndpointPlan::new("example.greeting@1", "1.0.0", ["greet"])
                        .with_cross_lane_transfer(),
                ),
        ],
        vec![CapabilityBinding::new(
            "consumer",
            "example.greeting@1",
            "1.0.0",
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
            ModuleInstancePlan::new("same-consumer", CONSUMER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("frontend"))
                .with_requirement(CapabilityRequirementPlan::one(
                    "example.greeting@1",
                    "1.0.0",
                )),
            ModuleInstancePlan::new("same-provider", GREETER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("frontend"))
                .with_capability(
                    CapabilityEndpointPlan::new("example.greeting@1", "1.0.0", ["greet"])
                        .with_cross_lane_transfer(),
                ),
            ModuleInstancePlan::new("cross-consumer", CONSUMER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("frontend"))
                .with_requirement(CapabilityRequirementPlan::one(
                    "example.greeting@1",
                    "1.0.0",
                )),
            ModuleInstancePlan::new("cross-provider", GREETER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("workers"))
                .with_capability(
                    CapabilityEndpointPlan::new("example.greeting@1", "1.0.0", ["greet"])
                        .with_cross_lane_transfer(),
                ),
        ],
        vec![
            CapabilityBinding::new(
                "same-consumer",
                "example.greeting@1",
                "1.0.0",
                "same-provider",
            ),
            CapabilityBinding::new(
                "cross-consumer",
                "example.greeting@1",
                "1.0.0",
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

fn greeting_adapters(lane: &ExecutionLaneId) -> ExecutionAdapterCatalog {
    let registry = match lane.as_str() {
        "frontend" => NativeModuleRegistry::new()
            .with_factory(ConsumerFactory)
            .with_factory(GreeterFactory),
        "workers" => NativeModuleRegistry::new().with_factory(GreeterFactory),
        other => panic!("unexpected lane {other}"),
    };
    ExecutionAdapterCatalog::single(registry)
}

fn greeting_transfers() -> CrossLaneRequestCatalog {
    CrossLaneRequestCatalog::new().with_request::<Greeting>(&[GREET_OPERATION])
}

fn slow_conformance_plan() -> lenso_app_plan::ResolvedAppPlan {
    AppComposition::new(
        vec![
            ModuleInstancePlan::new("same-consumer", EMPTY_CONSUMER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("frontend"))
                .with_requirement(CapabilityRequirementPlan::one(
                    "example.greeting@1",
                    "1.0.0",
                )),
            ModuleInstancePlan::new("same-provider", SLOW_GREETER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("frontend"))
                .with_capability(
                    CapabilityEndpointPlan::new("example.greeting@1", "1.0.0", ["greet"])
                        .with_limits(0, 1)
                        .with_cross_lane_transfer(),
                ),
            ModuleInstancePlan::new("cross-consumer", EMPTY_CONSUMER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("frontend"))
                .with_requirement(CapabilityRequirementPlan::one(
                    "example.greeting@1",
                    "1.0.0",
                )),
            ModuleInstancePlan::new("cross-provider", SLOW_GREETER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("workers"))
                .with_capability(
                    CapabilityEndpointPlan::new("example.greeting@1", "1.0.0", ["greet"])
                        .with_limits(0, 1)
                        .with_cross_lane_transfer(),
                ),
        ],
        vec![
            CapabilityBinding::new(
                "same-consumer",
                "example.greeting@1",
                "1.0.0",
                "same-provider",
            ),
            CapabilityBinding::new(
                "cross-consumer",
                "example.greeting@1",
                "1.0.0",
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
        "frontend" => NativeModuleRegistry::new()
            .with_factory(EmptyConsumerFactory)
            .with_factory(SlowGreeterFactory),
        "workers" => NativeModuleRegistry::new().with_factory(SlowGreeterFactory),
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
                "frontend" => NativeModuleRegistry::new().with_factory(ReportingConsumerFactory {
                    reported: reported.clone(),
                }),
                "workers" => NativeModuleRegistry::new().with_factory(GreeterFactory),
                other => panic!("unexpected lane {other}"),
            };
            ExecutionAdapterCatalog::single(registry)
        },
        greeting_transfers(),
    )
    .expect("both Kernel lanes should start");

    assert_eq!(app.lane_count(), 2);
    assert_eq!(
        report
            .recv_timeout(Duration::from_secs(1))
            .expect("the Module's generated client should invoke across lanes"),
        "Hello, module-client!"
    );
    let response = app
        .invoke::<Greeting>(
            "consumer",
            "greet",
            GreetRequest {
                name: "Ada".to_owned(),
            },
        )
        .await
        .expect("cross-lane invocation should reach the provider")
        .expect("provider should return success");
    assert_eq!(response.message, "Hello, Ada!");

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
            capability: "example.greeting@1".to_owned(),
        }
    );
}

#[tokio::test(flavor = "current_thread")]
async fn domain_errors_are_identical_on_same_and_cross_lane_bindings() {
    let app = ReplicatedNativeApp::start_with_transfers(
        same_and_cross_lane_plan(),
        greeting_adapters,
        greeting_transfers(),
    )
    .expect("both Kernel lanes should start");

    for caller in ["same-consumer", "cross-consumer"] {
        let outcome = app
            .invoke::<Greeting>(
                caller,
                "greet",
                GreetRequest {
                    name: String::new(),
                },
            )
            .await
            .expect("transport should preserve a Domain Error");
        assert_eq!(outcome, Err(GreetError::EmptyName));
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
        greeting_transfers(),
    )
    .expect("both Kernel lanes should start");

    for caller in ["same-consumer", "cross-consumer"] {
        let failure = app
            .invoke_with_options::<Greeting>(
                caller,
                "greet",
                GreetRequest {
                    name: "Ada".to_owned(),
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
        greeting_transfers(),
    )
    .expect("both Kernel lanes should start");

    for caller in ["same-consumer", "cross-consumer"] {
        let cancellation = LaneCancellationToken::new();
        let cancel_after_dispatch = cancellation.clone();
        let invoke = app.invoke_with_options::<Greeting>(
            caller,
            "greet",
            GreetRequest {
                name: "Ada".to_owned(),
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
        greeting_transfers(),
    )
    .expect("both Kernel lanes should start");

    for caller in ["same-consumer", "cross-consumer"] {
        let first = app.invoke::<Greeting>(
            caller,
            "greet",
            GreetRequest {
                name: "first".to_owned(),
            },
        );
        let second = app.invoke::<Greeting>(
            caller,
            "greet",
            GreetRequest {
                name: "second".to_owned(),
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
        greeting_adapters,
        greeting_transfers(),
    )
    .expect("both Kernel lanes should start");

    tokio::time::sleep(Duration::from_millis(20)).await;
    let baseline = app.diagnostics_snapshot();

    for caller in ["same-consumer", "cross-consumer"] {
        app.invoke::<Greeting>(
            caller,
            "greet",
            GreetRequest {
                name: "Ada".to_owned(),
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
