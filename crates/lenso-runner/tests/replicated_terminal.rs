use std::time::Duration;

use lenso_app_plan::{
    AppComposition, ExecutionLaneId, ExecutionLanePlan, ModuleCriticality, ModuleInstancePlan,
};
use lenso_kernel::{ActivateContext, ExecutionAdapterCatalog, ModuleLifecycle, RuntimeFailure};
use lenso_native_adapter::{
    NativeModuleFactory, NativeModuleFactoryContext, NativeModuleInstance, NativeModuleRegistry,
};
use lenso_runner::{ReplicatedNativeApp, ReplicatedRunnerError};

const FAILING_PACKAGE: &str = "fixture.terminal-failure";
const PEER_PACKAGE: &str = "fixture.terminal-peer";

#[derive(Debug)]
struct FailingFactory;

impl NativeModuleFactory for FailingFactory {
    fn package_id(&self) -> &'static str {
        FAILING_PACKAGE
    }

    fn instantiate(
        &self,
        _context: NativeModuleFactoryContext<'_>,
    ) -> Result<NativeModuleInstance, RuntimeFailure> {
        Ok(NativeModuleInstance::with_lifecycle(
            Vec::new(),
            FailingLifecycle,
        ))
    }
}

#[derive(Debug)]
struct FailingLifecycle;

impl ModuleLifecycle for FailingLifecycle {
    fn activate(&self, context: ActivateContext) -> lenso_kernel::ModuleFuture {
        context
            .tasks()
            .spawn_local(Box::pin(async { panic!("injected managed task failure") }))
            .expect("the failing managed task should be scheduled");
        Box::pin(async { Ok(()) })
    }
}

#[derive(Debug)]
struct PeerFactory;

impl NativeModuleFactory for PeerFactory {
    fn package_id(&self) -> &'static str {
        PEER_PACKAGE
    }

    fn instantiate(
        &self,
        _context: NativeModuleFactoryContext<'_>,
    ) -> Result<NativeModuleInstance, RuntimeFailure> {
        Ok(NativeModuleInstance::default())
    }
}

fn terminal_plan() -> lenso_app_plan::ResolvedAppPlan {
    AppComposition::new(
        vec![
            ModuleInstancePlan::new("failing", FAILING_PACKAGE)
                .with_execution_lane(ExecutionLaneId::new("failing-lane"))
                .with_criticality(ModuleCriticality::Critical),
            ModuleInstancePlan::new("peer", PEER_PACKAGE)
                .with_execution_lane(ExecutionLaneId::new("peer-lane")),
        ],
        Vec::new(),
    )
    .with_execution_lanes(vec![
        ExecutionLanePlan::new("failing-lane"),
        ExecutionLanePlan::new("peer-lane"),
    ])
    .resolve()
    .expect("the two-lane terminal fixture should resolve")
}

#[tokio::test(flavor = "current_thread")]
async fn kernel_terminal_failure_stops_every_replicated_lane_and_is_preserved() {
    let app = ReplicatedNativeApp::start(terminal_plan(), |_| {
        ExecutionAdapterCatalog::single(
            NativeModuleRegistry::new()
                .with_factory(FailingFactory)
                .with_factory(PeerFactory),
        )
    })
    .expect("both Kernel lanes should start before the managed task fails");

    let failure = tokio::time::timeout(Duration::from_secs(1), app.wait_for_terminal())
        .await
        .expect("the Kernel terminal failure should reach the Runner promptly");
    assert!(matches!(
        &failure,
        ReplicatedRunnerError::LaneRuntimeFailure {
            lane,
            error: RuntimeFailure::ModuleRestartExhausted {
                instance,
                attempts: 0,
            },
        } if lane == "failing-lane" && instance == "failing"
    ));
    assert_eq!(app.terminal_failure(), Some(failure.clone()));
    assert_eq!(app.shutdown(Duration::from_secs(1)).await, Err(failure));
}
