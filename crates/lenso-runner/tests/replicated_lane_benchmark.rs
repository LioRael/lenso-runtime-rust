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
use lenso_kernel::{ExecutionAdapterCatalog, InvocationContext, RuntimeFailure};
use lenso_runner::{CrossLaneRequestCatalog, ReplicatedNativeApp};
use lenso_runtime_conformance::{
    ConformanceExecutionAdapter, ConformanceModule, ConformanceModuleFactory, PROBE_CAPABILITY_ID,
    PROBE_DESCRIPTOR_VERSION, PROBE_OPERATION, Probe, ProbeEndpoint, ProbeInvocationError,
    ProbeProvider, ProbeProviderFactory, ProbeRequest, ProbeResponse,
};

const EMPTY_CONSUMER_PACKAGE_ID: &str = "fixture.empty-consumer";
const CPU_PROBE_PACKAGE_ID: &str = "fixture.cpu-probe";

#[derive(Debug)]
struct EmptyConsumerFactory;

impl ConformanceModuleFactory for EmptyConsumerFactory {
    fn package_id(&self) -> &'static str {
        EMPTY_CONSUMER_PACKAGE_ID
    }

    fn instantiate(
        &self,
        _instance: &ModuleInstancePlan,
    ) -> Result<ConformanceModule, RuntimeFailure> {
        Ok(ConformanceModule::default())
    }
}

#[derive(Debug)]
struct CpuProbeFactory;

impl ConformanceModuleFactory for CpuProbeFactory {
    fn package_id(&self) -> &'static str {
        CPU_PROBE_PACKAGE_ID
    }

    fn instantiate(
        &self,
        _instance: &ModuleInstancePlan,
    ) -> Result<ConformanceModule, RuntimeFailure> {
        Ok(ConformanceModule::new(vec![Rc::new(ProbeEndpoint::new(
            CpuProbe,
        ))]))
    }
}

#[derive(Debug)]
struct CpuProbe;

impl ProbeProvider for CpuProbe {
    fn probe(
        &self,
        _context: InvocationContext,
        request: ProbeRequest,
    ) -> LocalBoxFuture<'static, Result<ProbeResponse, ProbeInvocationError>> {
        Box::pin(async move {
            let cpu_started = ThreadTime::now();
            let mut accumulator = 0_u64;
            while cpu_started.elapsed() < Duration::from_millis(2) {
                accumulator = black_box(accumulator.wrapping_mul(31).wrapping_add(7));
            }
            black_box(accumulator);
            Ok(ProbeResponse {
                value: request.value,
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
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                )),
        );
        instances.push(
            ModuleInstancePlan::new(&provider, CPU_PROBE_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new(&lane))
                .with_capability(CapabilityEndpointPlan::new(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                    [PROBE_OPERATION],
                )),
        );
        bindings.push(CapabilityBinding::new(
            consumer,
            PROBE_CAPABILITY_ID,
            PROBE_DESCRIPTOR_VERSION,
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
        ConformanceExecutionAdapter::new()
            .with_factory(EmptyConsumerFactory)
            .with_factory(CpuProbeFactory),
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
                app.invoke::<Probe>(
                    &caller,
                    PROBE_OPERATION,
                    ProbeRequest {
                        value: request.to_string(),
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
    f64::from(u32::try_from(requests).expect("benchmark request count fits u32"))
        / elapsed.as_secs_f64()
}

fn request_transfer_plan(cross_lane: bool) -> lenso_app_plan::ResolvedAppPlan {
    let provider_lane = if cross_lane { "workers" } else { "frontend" };
    AppComposition::new(
        vec![
            ModuleInstancePlan::new("consumer", EMPTY_CONSUMER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("frontend"))
                .with_requirement(CapabilityRequirementPlan::one(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                )),
            ModuleInstancePlan::new(
                "provider",
                lenso_runtime_conformance::PROBE_PROVIDER_PACKAGE_ID,
            )
            .with_execution_lane(ExecutionLaneId::new(provider_lane))
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
    .expect("request transfer benchmark Plan should resolve")
}

fn request_adapters(lane: &ExecutionLaneId) -> ExecutionAdapterCatalog {
    let registry = match lane.as_str() {
        "frontend" => ConformanceExecutionAdapter::new()
            .with_factory(EmptyConsumerFactory)
            .with_factory(ProbeProviderFactory),
        "workers" => ConformanceExecutionAdapter::new().with_factory(ProbeProviderFactory),
        other => panic!("unexpected lane {other}"),
    };
    ExecutionAdapterCatalog::single(registry)
}

async fn measure_request_throughput(cross_lane: bool, requests: usize) -> f64 {
    let app = ReplicatedNativeApp::start_with_transfers(
        request_transfer_plan(cross_lane),
        request_adapters,
        CrossLaneRequestCatalog::new().with_request::<Probe>(&[PROBE_OPERATION]),
    )
    .expect("request benchmark lanes should start");
    let started = Instant::now();
    for _ in 0..requests {
        app.invoke::<Probe>(
            "consumer",
            PROBE_OPERATION,
            ProbeRequest {
                value: "benchmark".to_owned(),
            },
        )
        .await
        .expect("benchmark transport should succeed")
        .expect("benchmark provider should succeed");
    }
    let elapsed = started.elapsed();
    app.shutdown(Duration::from_secs(2))
        .await
        .expect("benchmark lanes should stop cleanly");
    f64::from(u32::try_from(requests).expect("benchmark request count fits u32"))
        / elapsed.as_secs_f64()
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

/// Reproducible evidence command:
/// `lenso-cargo test -p lenso-runner --test replicated_lane_benchmark request_transfer_benchmark -- --ignored --nocapture`
#[tokio::test(flavor = "current_thread")]
#[ignore = "request transport benchmark; run explicitly when changing lane scheduling"]
async fn request_transfer_benchmark() {
    let requests = 100_000;
    let mut same_lane_samples = Vec::with_capacity(5);
    let mut cross_lane_samples = Vec::with_capacity(5);
    for _ in 0..5 {
        same_lane_samples.push(measure_request_throughput(false, requests).await);
        cross_lane_samples.push(measure_request_throughput(true, requests).await);
    }
    same_lane_samples.sort_by(f64::total_cmp);
    cross_lane_samples.sort_by(f64::total_cmp);
    let same_lane = same_lane_samples[same_lane_samples.len() / 2];
    let cross_lane = cross_lane_samples[cross_lane_samples.len() / 2];
    println!(
        "{{\"requests_per_sample\":{requests},\"samples\":5,\"same_lane_per_second\":{same_lane:.3},\"cross_lane_per_second\":{cross_lane:.3},\"cross_lane_ratio\":{:.3}}}",
        cross_lane / same_lane,
    );
}
