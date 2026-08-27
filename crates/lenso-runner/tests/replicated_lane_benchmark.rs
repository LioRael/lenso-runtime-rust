use std::{
    hint::black_box,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};

use cpu_time::ThreadTime;
use futures::future::{LocalBoxFuture, join_all};
use lenso_app_plan::{
    AppComposition, CapabilityBinding, CapabilityEndpointPlan, CapabilityRequirementPlan,
    ExecutionLaneId, ExecutionLanePlan, PluginInstancePlan,
};
use lenso_kernel::{
    ActivateContext, ExecutionAdapterCatalog, InvocationContext, PluginLifecycle, RuntimeFailure,
};
use lenso_runner::{CrossLaneRequestCatalog, ReplicatedNativeApp};
use lenso_runtime_conformance::{
    ConformanceExecutionAdapter, ConformancePlugin, ConformancePluginFactory, PROBE_CAPABILITY_ID,
    PROBE_DESCRIPTOR_VERSION, PROBE_OPERATION, Probe, ProbeEndpoint, ProbeInvocationError,
    ProbeProvider, ProbeProviderFactory, ProbeRequest, ProbeResponse,
};

const EMPTY_CONSUMER_PACKAGE_ID: &str = "fixture.empty-consumer";
const CPU_PROBE_PACKAGE_ID: &str = "fixture.cpu-probe";
const BENCHMARK_CONSUMER_PACKAGE_ID: &str = "fixture.benchmark-consumer";
const CROSS_LANE_SAMPLE_ORDER: [bool; 10] = [
    false, true, true, false, true, false, false, true, false, true,
];

#[derive(Debug)]
struct EmptyConsumerFactory;

impl ConformancePluginFactory for EmptyConsumerFactory {
    fn package_id(&self) -> &'static str {
        EMPTY_CONSUMER_PACKAGE_ID
    }

    fn instantiate(
        &self,
        _instance: &PluginInstancePlan,
    ) -> Result<ConformancePlugin, RuntimeFailure> {
        Ok(ConformancePlugin::default())
    }
}

#[derive(Debug)]
struct CpuProbeFactory;

impl ConformancePluginFactory for CpuProbeFactory {
    fn package_id(&self) -> &'static str {
        CPU_PROBE_PACKAGE_ID
    }

    fn instantiate(
        &self,
        _instance: &PluginInstancePlan,
    ) -> Result<ConformancePlugin, RuntimeFailure> {
        Ok(ConformancePlugin::new(vec![Rc::new(ProbeEndpoint::new(
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

#[derive(Debug)]
struct BenchmarkConsumerFactory {
    requests: usize,
    concurrency: usize,
    reported: mpsc::Sender<f64>,
}

impl ConformancePluginFactory for BenchmarkConsumerFactory {
    fn package_id(&self) -> &'static str {
        BENCHMARK_CONSUMER_PACKAGE_ID
    }

    fn instantiate(
        &self,
        _instance: &PluginInstancePlan,
    ) -> Result<ConformancePlugin, RuntimeFailure> {
        Ok(ConformancePlugin::with_lifecycle(
            Vec::new(),
            BenchmarkConsumerLifecycle {
                requests: self.requests,
                concurrency: self.concurrency,
                reported: self.reported.clone(),
            },
        ))
    }
}

#[derive(Debug)]
struct BenchmarkConsumerLifecycle {
    requests: usize,
    concurrency: usize,
    reported: mpsc::Sender<f64>,
}

impl PluginLifecycle for BenchmarkConsumerLifecycle {
    fn activate(&self, context: ActivateContext) -> lenso_kernel::PluginFuture {
        let client =
            lenso_runtime_conformance::ProbeClient::from_dependencies(context.dependencies());
        let requests = self.requests;
        let concurrency = self.concurrency;
        let reported = self.reported.clone();
        Box::pin(async move {
            let client = client?;
            let started = Instant::now();
            if concurrency == 1 {
                for _ in 0..requests {
                    client
                        .probe(ProbeRequest {
                            value: "benchmark".to_owned(),
                        })
                        .await
                        .map_err(|error| benchmark_client_failure(&error))?;
                }
            } else {
                for batch_start in (0..requests).step_by(concurrency) {
                    let batch_len = concurrency.min(requests - batch_start);
                    let outcomes = join_all((0..batch_len).map(|_| {
                        client.probe(ProbeRequest {
                            value: "benchmark".to_owned(),
                        })
                    }))
                    .await;
                    for outcome in outcomes {
                        outcome.map_err(|error| benchmark_client_failure(&error))?;
                    }
                }
            }
            let throughput =
                f64::from(u32::try_from(requests).expect("benchmark request count fits u32"))
                    / started.elapsed().as_secs_f64();
            let _ = reported.send(throughput);
            Ok(())
        })
    }
}

fn benchmark_client_failure(error: &ProbeInvocationError) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: format!("typed benchmark client failed: {error:?}"),
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
            PluginInstancePlan::new(&consumer, EMPTY_CONSUMER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new(&lane))
                .with_requirement(CapabilityRequirementPlan::one(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                )),
        );
        instances.push(
            PluginInstancePlan::new(&provider, CPU_PROBE_PACKAGE_ID)
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
            PluginInstancePlan::new("consumer", EMPTY_CONSUMER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("frontend"))
                .with_requirement(CapabilityRequirementPlan::one(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                )),
            PluginInstancePlan::new(
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

fn plugin_request_transfer_plan(
    cross_lane: bool,
    concurrency: usize,
) -> lenso_app_plan::ResolvedAppPlan {
    let provider_lane = if cross_lane { "workers" } else { "frontend" };
    AppComposition::new(
        vec![
            PluginInstancePlan::new("consumer", BENCHMARK_CONSUMER_PACKAGE_ID)
                .with_execution_lane(ExecutionLaneId::new("frontend"))
                .with_requirement(CapabilityRequirementPlan::one(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                )),
            PluginInstancePlan::new(
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
                .with_limits(concurrency, concurrency)
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
    .expect("module request transfer benchmark Plan should resolve")
}

async fn measure_plugin_request_throughput(
    cross_lane: bool,
    requests: usize,
    concurrency: usize,
) -> f64 {
    let (reported, report) = mpsc::channel();
    let app = ReplicatedNativeApp::start_with_transfers(
        plugin_request_transfer_plan(cross_lane, concurrency),
        move |lane| {
            let registry = match lane.as_str() {
                "frontend" => ConformanceExecutionAdapter::new()
                    .with_factory(BenchmarkConsumerFactory {
                        requests,
                        concurrency,
                        reported: reported.clone(),
                    })
                    .with_factory(ProbeProviderFactory),
                "workers" => ConformanceExecutionAdapter::new().with_factory(ProbeProviderFactory),
                other => panic!("unexpected lane {other}"),
            };
            ExecutionAdapterCatalog::single(registry)
        },
        CrossLaneRequestCatalog::new().with_request::<Probe>(&[PROBE_OPERATION]),
    )
    .expect("module request transfer benchmark lanes should start");
    let throughput = report
        .recv_timeout(Duration::from_secs(60))
        .expect("benchmark consumer should report throughput");
    app.shutdown(Duration::from_secs(2))
        .await
        .expect("benchmark lanes should stop cleanly");
    throughput
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

async fn measure_interleaved_plugin_samples(
    requests: usize,
    concurrency: usize,
) -> (Vec<f64>, Vec<f64>) {
    let mut same_lane = Vec::with_capacity(5);
    let mut cross_lane = Vec::with_capacity(5);
    for cross_lane_sample in CROSS_LANE_SAMPLE_ORDER {
        let throughput =
            measure_plugin_request_throughput(cross_lane_sample, requests, concurrency).await;
        if cross_lane_sample {
            cross_lane.push(throughput);
        } else {
            same_lane.push(throughput);
        }
    }
    (same_lane, cross_lane)
}

async fn measure_interleaved_external_samples(requests: usize) -> (Vec<f64>, Vec<f64>) {
    let mut same_lane = Vec::with_capacity(5);
    let mut cross_lane = Vec::with_capacity(5);
    for cross_lane_sample in CROSS_LANE_SAMPLE_ORDER {
        let throughput = measure_request_throughput(cross_lane_sample, requests).await;
        if cross_lane_sample {
            cross_lane.push(throughput);
        } else {
            same_lane.push(throughput);
        }
    }
    (same_lane, cross_lane)
}

fn median(samples: &[f64]) -> f64 {
    let mut ordered = samples.to_vec();
    ordered.sort_by(f64::total_cmp);
    ordered[ordered.len() / 2]
}

fn print_interleaved_report(
    requests: usize,
    concurrency: Option<usize>,
    same_lane_samples: &[f64],
    cross_lane_samples: &[f64],
) {
    let same_lane = median(same_lane_samples);
    let cross_lane = median(cross_lane_samples);
    let concurrency =
        concurrency.map_or_else(String::new, |value| format!("\"concurrency\":{value},"));
    println!(
        "{{\"requests_per_sample\":{requests},\"samples_per_path\":5,{concurrency}\"cross_lane_sample_order\":{CROSS_LANE_SAMPLE_ORDER:?},\"same_lane_samples_per_second\":{same_lane_samples:?},\"cross_lane_samples_per_second\":{cross_lane_samples:?},\"same_lane_median_per_second\":{same_lane:.3},\"cross_lane_median_per_second\":{cross_lane:.3},\"cross_lane_ratio\":{:.3}}}",
        cross_lane / same_lane,
    );
}

/// Reproducible evidence command:
/// `cargo test -p lenso-runner --test replicated_lane_benchmark lane_scaling_benchmark -- --ignored --nocapture`
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
/// `cargo test --release -p lenso-runner --test replicated_lane_benchmark request_transfer_benchmark -- --exact --ignored --nocapture`
#[tokio::test(flavor = "current_thread")]
#[ignore = "module request transfer benchmark; run explicitly when changing lane scheduling"]
async fn request_transfer_benchmark() {
    let requests = 100_000;
    let (same_lane_samples, cross_lane_samples) =
        measure_interleaved_plugin_samples(requests, 1).await;
    print_interleaved_report(requests, Some(1), &same_lane_samples, &cross_lane_samples);
}

/// Reproducible evidence command:
/// `cargo test --release -p lenso-runner --test replicated_lane_benchmark concurrent_request_transfer_benchmark -- --exact --ignored --nocapture`
#[tokio::test(flavor = "current_thread")]
#[ignore = "concurrent module request benchmark; run explicitly when changing lane scheduling"]
async fn concurrent_request_transfer_benchmark() {
    let requests = 100_000;
    let concurrency = 64;
    let (same_lane_samples, cross_lane_samples) =
        measure_interleaved_plugin_samples(requests, concurrency).await;
    print_interleaved_report(
        requests,
        Some(concurrency),
        &same_lane_samples,
        &cross_lane_samples,
    );
}

/// Reproducible evidence command:
/// `cargo test --release -p lenso-runner --test replicated_lane_benchmark external_request_routing_benchmark -- --exact --ignored --nocapture`
#[tokio::test(flavor = "current_thread")]
#[ignore = "external request routing benchmark; run explicitly when changing lane scheduling"]
async fn external_request_routing_benchmark() {
    let requests = 100_000;
    let (same_lane_samples, cross_lane_samples) =
        measure_interleaved_external_samples(requests).await;
    print_interleaved_report(requests, None, &same_lane_samples, &cross_lane_samples);
}
