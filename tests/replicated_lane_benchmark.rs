use std::{
    hint::black_box,
    rc::Rc,
    time::{Duration, Instant},
};

use cpu_time::ThreadTime;
use futures::future::{LocalBoxFuture, join_all};
use lenso_app_plan::{
    AppComposition, CapabilityBinding, CapabilityEndpointPlan, CapabilityRequirementPlan,
    ExecutionLaneId, ExecutionLanePlan, ModuleInstancePlan,
};
use lenso_capability_greeting::{
    GreetRequest, GreetResponse, Greeting, GreetingEndpoint, GreetingInvocationError,
    GreetingProvider,
};
use lenso_kernel::{ExecutionAdapterCatalog, InvocationContext, RuntimeFailure};
use lenso_native_adapter::{
    NativeModuleFactory, NativeModuleFactoryContext, NativeModuleInstance, NativeModuleRegistry,
};
use lenso_runner::ReplicatedNativeApp;

const EMPTY_CONSUMER_PACKAGE_ID: &str = "fixture.empty-consumer";
const CPU_GREETER_PACKAGE_ID: &str = "fixture.cpu-greeter";

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
struct CpuGreeterFactory;

impl NativeModuleFactory for CpuGreeterFactory {
    fn package_id(&self) -> &'static str {
        CPU_GREETER_PACKAGE_ID
    }

    fn instantiate(
        &self,
        _context: NativeModuleFactoryContext<'_>,
    ) -> Result<NativeModuleInstance, RuntimeFailure> {
        Ok(NativeModuleInstance::new(vec![Rc::new(
            GreetingEndpoint::new(CpuGreeter),
        )]))
    }
}

#[derive(Debug)]
struct CpuGreeter;

impl GreetingProvider for CpuGreeter {
    fn greet(
        &self,
        _context: InvocationContext,
        request: GreetRequest,
    ) -> LocalBoxFuture<'static, Result<GreetResponse, GreetingInvocationError>> {
        Box::pin(async move {
            let cpu_started = ThreadTime::now();
            let mut accumulator = 0_u64;
            while cpu_started.elapsed() < Duration::from_millis(2) {
                accumulator = black_box(accumulator.wrapping_mul(31).wrapping_add(7));
            }
            black_box(accumulator);
            Ok(GreetResponse {
                message: request.name,
            })
        })
    }
}

fn shared_nothing_plan(lane_count: usize) -> lenso_app_plan::ResolvedAppPlan {
    let mut instances = Vec::with_capacity(lane_count * 2);
    let mut bindings = Vec::with_capacity(lane_count);
    let mut lanes = Vec::with_capacity(lane_count);
    for index in 0..lane_count {
        let lane = format!("lane-{index}");
        let consumer = format!("consumer-{index}");
        let provider = format!("provider-{index}");
        lanes.push(ExecutionLanePlan::new(&lane));
        instances.push(
            ModuleInstancePlan::new(&consumer, EMPTY_CONSUMER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new(&lane))
                .with_requirement(CapabilityRequirementPlan::one(
                    "example.greeting@1",
                    "1.0.0",
                )),
        );
        instances.push(
            ModuleInstancePlan::new(&provider, CPU_GREETER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new(&lane))
                .with_capability(CapabilityEndpointPlan::new(
                    "example.greeting@1",
                    "1.0.0",
                    ["greet"],
                )),
        );
        bindings.push(CapabilityBinding::new(
            consumer,
            "example.greeting@1",
            "1.0.0",
            provider,
        ));
    }
    AppComposition::new(instances, bindings)
        .with_execution_lanes(lanes)
        .resolve()
        .expect("shared-nothing benchmark Plan should resolve")
}

fn cpu_adapters(_lane: &ExecutionLaneId) -> ExecutionAdapterCatalog {
    ExecutionAdapterCatalog::single(
        NativeModuleRegistry::new()
            .with_factory(EmptyConsumerFactory)
            .with_factory(CpuGreeterFactory),
    )
}

async fn measure_shared_nothing_throughput(lane_count: usize, requests: usize) -> f64 {
    let app = ReplicatedNativeApp::start(shared_nothing_plan(lane_count), cpu_adapters)
        .expect("benchmark lanes should start");
    let requests_per_lane = requests / lane_count;
    let started = Instant::now();
    let lane_runs = (0..lane_count).map(|index| {
        let app = &app;
        async move {
            let caller = format!("consumer-{index}");
            for request in 0..requests_per_lane {
                app.invoke::<Greeting>(
                    &caller,
                    "greet",
                    GreetRequest {
                        name: request.to_string(),
                    },
                )
                .await
                .expect("benchmark transport should succeed")
                .expect("benchmark provider should succeed");
            }
        }
    });
    join_all(lane_runs).await;
    let elapsed = started.elapsed();
    app.shutdown(Duration::from_secs(2))
        .await
        .expect("benchmark lanes should stop cleanly");
    requests as f64 / elapsed.as_secs_f64()
}

/// Reproducible evidence command:
/// `lenso-cargo test -p lenso-runner --test replicated_lane_benchmark lane_scaling_benchmark -- --ignored --nocapture`
#[tokio::test(flavor = "current_thread")]
#[ignore = "CPU benchmark; run explicitly to refresh docs/evidence/native-lane-scaling.json"]
async fn lane_scaling_benchmark() {
    let requests = 120;
    let one = measure_shared_nothing_throughput(1, requests).await;
    let two = measure_shared_nothing_throughput(2, requests).await;
    let four = measure_shared_nothing_throughput(4, requests).await;
    println!(
        "{{\"requests\":{requests},\"results\":[{{\"lanes\":1,\"throughput_per_second\":{one:.3},\"speedup\":1.000}},{{\"lanes\":2,\"throughput_per_second\":{two:.3},\"speedup\":{:.3}}},{{\"lanes\":4,\"throughput_per_second\":{four:.3},\"speedup\":{:.3}}}]}}",
        two / one,
        four / one,
    );
    assert!(two / one >= 1.6, "two lanes should scale near-linearly");
    assert!(four / one >= 3.0, "four lanes should scale near-linearly");
}
