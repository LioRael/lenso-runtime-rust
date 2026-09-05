#![cfg(feature = "test-fixture")]

use std::{
    any::Any,
    collections::BTreeMap,
    fs,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use lenso_app_plan::{
    AppComposition, CapabilityBinding, CapabilityEndpointPlan, CapabilityRequirementPlan,
    ExecutionClassId, PluginInstancePlan, TerminalPolicy,
};
use lenso_kernel::{
    CancellationToken, DeterministicDriver, ExecutionAdapterCatalog, InvocationContext, Kernel,
    NativeRequestEndpoint, PluginDependencyHandle, RequestCapability, RuntimeDriver,
    RuntimeFailure,
};
use lenso_native_adapter::{
    NativePluginFactory, NativePluginFactoryContext, NativePluginInstance, NativePluginRegistry,
};
use lenso_process_adapter::{
    AuthoringProcessAdapter, EXECUTION_CLASS, ProcessAdapter, ProcessLimits, RUNTIME_PROFILE_V2,
};
use lenso_runtime_codec::{
    ArtifactCatalog, ArtifactHandle, JsonCapabilityCodec, JsonHostRequestFuture,
    JsonInvocationOutcome,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

const STORE_ID: &str = "example.document-store@1";
const SYNC_ID: &str = "example.sync@1";
const DESCRIPTOR_VERSION: &str = "1.0.0";
const STORE_DIGEST: &str =
    "sha256:1100000000000000000000000000000000000000000000000000000000000011";
const SYNC_DIGEST: &str = "sha256:2200000000000000000000000000000000000000000000000000000000000022";
const LANGUAGE_EXECUTION_CLASS: &str = "example.language@1";

#[derive(Debug)]
struct Store;

impl RequestCapability for Store {
    type Request = Value;
    type Response = Value;
    type DomainError = Value;

    const ID: &'static str = STORE_ID;
    const DESCRIPTOR_VERSION: &'static str = DESCRIPTOR_VERSION;
}

#[derive(Debug)]
struct Sync;

impl RequestCapability for Sync {
    type Request = Value;
    type Response = Value;
    type DomainError = Value;

    const ID: &'static str = SYNC_ID;
    const DESCRIPTOR_VERSION: &'static str = DESCRIPTOR_VERSION;
}

#[derive(Debug)]
struct StoreEndpoint {
    state: Arc<Mutex<BTreeMap<String, String>>>,
    calls: Arc<AtomicUsize>,
}

impl NativeRequestEndpoint for StoreEndpoint {
    fn capability_id(&self) -> &'static str {
        STORE_ID
    }

    fn descriptor_version(&self) -> &'static str {
        DESCRIPTOR_VERSION
    }

    fn operations(&self) -> &'static [&'static str] {
        &["put", "read"]
    }

    fn invoke(
        &self,
        operation: &str,
        request: Box<dyn Any>,
        _context: InvocationContext,
    ) -> futures::future::LocalBoxFuture<
        'static,
        Result<Result<Box<dyn Any>, Box<dyn Any>>, RuntimeFailure>,
    > {
        let operation = operation.to_owned();
        let state = self.state.clone();
        let calls = self.calls.clone();
        Box::pin(async move {
            let request =
                request
                    .downcast::<Value>()
                    .map_err(|_| RuntimeFailure::ProtocolViolation {
                        capability: STORE_ID,
                    })?;
            let document = request
                .get("document")
                .and_then(Value::as_str)
                .ok_or(RuntimeFailure::ProtocolViolation {
                    capability: STORE_ID,
                })?
                .to_owned();
            calls.fetch_add(1, Ordering::Relaxed);
            let mut state = state.lock().expect("Store state");
            let response = match operation.as_str() {
                "read" => json!({"text": state.get(&document).cloned().unwrap_or_default()}),
                "put" => {
                    let text = request
                        .get("text")
                        .and_then(Value::as_str)
                        .ok_or(RuntimeFailure::ProtocolViolation {
                            capability: STORE_ID,
                        })?
                        .to_owned();
                    state.insert(document, text);
                    json!({"stored": true})
                }
                _ => {
                    return Err(RuntimeFailure::UnknownOperation {
                        capability: STORE_ID,
                        operation,
                    });
                }
            };
            Ok(Ok(Box::new(response) as Box<dyn Any>))
        })
    }
}

#[derive(Debug)]
struct StoreFactory {
    package: &'static str,
    state: Arc<Mutex<BTreeMap<String, String>>>,
    calls: Arc<AtomicUsize>,
}

impl NativePluginFactory for StoreFactory {
    fn package_id(&self) -> &'static str {
        self.package
    }

    fn instantiate(
        &self,
        _context: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        Ok(NativePluginInstance::new(vec![Rc::new(StoreEndpoint {
            state: self.state.clone(),
            calls: self.calls.clone(),
        })]))
    }
}

#[derive(Debug)]
struct EmptyConsumerFactory;

impl NativePluginFactory for EmptyConsumerFactory {
    fn package_id(&self) -> &'static str {
        "test.sync-consumer"
    }

    fn instantiate(
        &self,
        _context: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        Ok(NativePluginInstance::default())
    }
}

#[derive(Debug)]
struct StoreCodec;

impl JsonCapabilityCodec for StoreCodec {
    fn capability_id(&self) -> &'static str {
        STORE_ID
    }

    fn descriptor_version(&self) -> &'static str {
        DESCRIPTOR_VERSION
    }

    fn descriptor_digest(&self) -> &'static str {
        STORE_DIGEST
    }

    fn request_operations(&self) -> &'static [&'static str] {
        &["put", "read"]
    }

    fn encode_request(&self, _: &str, request: &dyn Any) -> Result<Value, RuntimeFailure> {
        request
            .downcast_ref::<Value>()
            .cloned()
            .ok_or(RuntimeFailure::ProtocolViolation {
                capability: STORE_ID,
            })
    }

    fn decode_response(&self, _: &str, value: Value) -> Result<Box<dyn Any>, RuntimeFailure> {
        Ok(Box::new(value))
    }

    fn decode_domain_error(&self, _: &str, value: Value) -> Result<Box<dyn Any>, RuntimeFailure> {
        Ok(Box::new(value))
    }

    fn invoke_host_request(
        &self,
        dependency: PluginDependencyHandle,
        operation: String,
        request: Value,
        context: InvocationContext,
    ) -> JsonHostRequestFuture {
        Box::pin(async move {
            match dependency
                .typed::<Store>()?
                .invoke_with_context(&operation, context, request)
                .await?
            {
                Ok(value) => Ok(JsonInvocationOutcome::Success(value)),
                Err(error) => Ok(JsonInvocationOutcome::DomainError(error)),
            }
        })
    }
}

#[derive(Debug)]
struct SyncCodec;

impl JsonCapabilityCodec for SyncCodec {
    fn capability_id(&self) -> &'static str {
        SYNC_ID
    }

    fn descriptor_version(&self) -> &'static str {
        DESCRIPTOR_VERSION
    }

    fn descriptor_digest(&self) -> &'static str {
        SYNC_DIGEST
    }

    fn request_operations(&self) -> &'static [&'static str] {
        &["sync"]
    }

    fn encode_request(&self, _: &str, request: &dyn Any) -> Result<Value, RuntimeFailure> {
        request
            .downcast_ref::<Value>()
            .cloned()
            .ok_or(RuntimeFailure::ProtocolViolation {
                capability: SYNC_ID,
            })
    }

    fn decode_response(&self, _: &str, value: Value) -> Result<Box<dyn Any>, RuntimeFailure> {
        Ok(Box::new(value))
    }

    fn decode_domain_error(&self, _: &str, value: Value) -> Result<Box<dyn Any>, RuntimeFailure> {
        Ok(Box::new(value))
    }
}

#[test]
fn process_v2_calls_two_named_native_store_dependencies_and_stops_cleanly() {
    let source = Arc::new(Mutex::new(BTreeMap::from([(
        "guide".to_owned(),
        "complete object".to_owned(),
    )])));
    let destination = Arc::new(Mutex::new(BTreeMap::new()));
    let source_calls = Arc::new(AtomicUsize::new(0));
    let destination_calls = Arc::new(AtomicUsize::new(0));
    let (driver, app) = start_process_app(&source, &destination, &source_calls, &destination_calls);
    let result = driver
        .run(
            app.handle::<Sync>("consumer")
                .unwrap()
                .invoke("sync", json!({"document": "guide"})),
        )
        .unwrap()
        .unwrap();

    assert_eq!(
        result,
        json!({"document": "guide", "text": "complete object"})
    );
    assert_eq!(
        destination.lock().unwrap().get("guide").map(String::as_str),
        Some("complete object")
    );
    assert_eq!(source.lock().unwrap().len(), 1);
    assert_eq!(source_calls.load(Ordering::Relaxed), 1);
    assert_eq!(destination_calls.load(Ordering::Relaxed), 1);
    assert!(matches!(
        driver.run(app.shutdown(Duration::from_secs(1))),
        lenso_kernel::ShutdownOutcome::Clean
    ));
}

#[test]
fn process_v2_lifecycle_scopes_can_call_declared_dependencies() {
    let source = Arc::new(Mutex::new(BTreeMap::from([(
        "startup".to_owned(),
        "ready".to_owned(),
    )])));
    let destination = Arc::new(Mutex::new(BTreeMap::new()));
    let source_calls = Arc::new(AtomicUsize::new(0));
    let destination_calls = Arc::new(AtomicUsize::new(0));
    let (driver, app) = start_process_app_with_engine(
        &source,
        &destination,
        &source_calls,
        &destination_calls,
        ProcessLimits::default(),
        false,
        true,
    );

    assert_eq!(source_calls.load(Ordering::Relaxed), 1);
    assert!(matches!(
        driver.run(app.shutdown(Duration::from_secs(1))),
        lenso_kernel::ShutdownOutcome::Clean
    ));
    assert_eq!(destination_calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        destination
            .lock()
            .unwrap()
            .get("cleanup")
            .map(String::as_str),
        Some("stopped")
    );
}

#[test]
fn reusable_authoring_engine_runs_a_language_owned_execution_class() {
    let source = Arc::new(Mutex::new(BTreeMap::from([(
        "guide".to_owned(),
        "shared transport".to_owned(),
    )])));
    let destination = Arc::new(Mutex::new(BTreeMap::new()));
    let source_calls = Arc::new(AtomicUsize::new(0));
    let destination_calls = Arc::new(AtomicUsize::new(0));
    let (driver, app) = start_process_app_with_engine(
        &source,
        &destination,
        &source_calls,
        &destination_calls,
        ProcessLimits::default(),
        true,
        false,
    );

    let result = driver
        .run(
            app.handle::<Sync>("consumer")
                .unwrap()
                .invoke("sync", json!({"document": "guide"})),
        )
        .unwrap()
        .unwrap();

    assert_eq!(
        result,
        json!({"document": "guide", "text": "shared transport"})
    );
    assert!(matches!(
        driver.run(app.shutdown(Duration::from_secs(1))),
        lenso_kernel::ShutdownOutcome::Clean
    ));
}

#[test]
fn forged_route_is_rejected_before_the_native_provider_is_dispatched() {
    let source = Arc::new(Mutex::new(BTreeMap::from([(
        "guide".to_owned(),
        "secret".to_owned(),
    )])));
    let destination = Arc::new(Mutex::new(BTreeMap::new()));
    let source_calls = Arc::new(AtomicUsize::new(0));
    let destination_calls = Arc::new(AtomicUsize::new(0));
    let (driver, app) = start_process_app(&source, &destination, &source_calls, &destination_calls);

    let error = driver
        .run(
            app.handle::<Sync>("consumer")
                .unwrap()
                .invoke("sync", json!({"document": "guide", "forge_route": true})),
        )
        .unwrap_err();

    assert!(matches!(error, RuntimeFailure::ProtocolViolation { .. }));
    assert_eq!(source_calls.load(Ordering::Relaxed), 0);
    assert_eq!(destination_calls.load(Ordering::Relaxed), 0);
}

#[test]
fn ignored_cancellation_keeps_capacity_until_the_process_is_killed_and_reaped() {
    let source = Arc::new(Mutex::new(BTreeMap::new()));
    let destination = Arc::new(Mutex::new(BTreeMap::new()));
    let source_calls = Arc::new(AtomicUsize::new(0));
    let destination_calls = Arc::new(AtomicUsize::new(0));
    let limits = ProcessLimits {
        max_pending_requests: 1,
        cancellation_settlement_timeout: Duration::from_millis(50),
        ..ProcessLimits::default()
    };
    let (driver, app) = start_process_app_with_limits(
        &source,
        &destination,
        &source_calls,
        &destination_calls,
        limits,
    );
    let handle = app.handle::<Sync>("consumer").unwrap();
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    let blocked = handle.invoke_with_context(
        "sync",
        InvocationContext::new(40, None, cancellation),
        json!({"document": "guide", "sleep_ms": 5_000}),
    );
    let second = async {
        cancel.cancel();
        handle
            .invoke_with_context(
                "sync",
                InvocationContext::new(41, None, CancellationToken::new()),
                json!({"document": "guide"}),
            )
            .await
    };

    let started = Instant::now();
    let (blocked_result, second_result) = driver.run(futures::future::join(blocked, second));

    assert!(matches!(
        blocked_result,
        Err(RuntimeFailure::Cancelled { request_id: 40 })
    ));
    assert_eq!(
        second_result,
        Err(RuntimeFailure::Unavailable {
            capability: SYNC_ID,
        })
    );
    assert!(started.elapsed() >= Duration::from_millis(30));
    assert_eq!(source_calls.load(Ordering::Relaxed), 0);
    assert_eq!(destination_calls.load(Ordering::Relaxed), 0);
}

#[test]
fn unexpected_process_exit_reports_a_host_essential_terminal_failure() {
    let source = Arc::new(Mutex::new(BTreeMap::new()));
    let destination = Arc::new(Mutex::new(BTreeMap::new()));
    let source_calls = Arc::new(AtomicUsize::new(0));
    let destination_calls = Arc::new(AtomicUsize::new(0));
    let (driver, app) = start_process_app_with_configuration(
        &source,
        &destination,
        &source_calls,
        &destination_calls,
        ProcessLimits::default(),
        false,
        &json!({"exit_after_construct_ms": 25}),
        true,
    );

    std::thread::sleep(Duration::from_millis(75));
    for _ in 0..8 {
        driver.run(async { driver.yield_now().await });
    }

    assert!(app.is_failed());
    assert!(!app.is_accepting());
    let terminal = app.terminal_failure();
    assert!(
        matches!(
        terminal,
        Some(RuntimeFailure::PluginRestartExhausted { ref instance, .. }) if instance == "sync"
        ),
        "terminal failure: {terminal:?}"
    );
}

fn start_process_app(
    source: &Arc<Mutex<BTreeMap<String, String>>>,
    destination: &Arc<Mutex<BTreeMap<String, String>>>,
    source_calls: &Arc<AtomicUsize>,
    destination_calls: &Arc<AtomicUsize>,
) -> (DeterministicDriver, lenso_kernel::NativeApp) {
    start_process_app_with_limits(
        source,
        destination,
        source_calls,
        destination_calls,
        ProcessLimits::default(),
    )
}

fn start_process_app_with_limits(
    source: &Arc<Mutex<BTreeMap<String, String>>>,
    destination: &Arc<Mutex<BTreeMap<String, String>>>,
    source_calls: &Arc<AtomicUsize>,
    destination_calls: &Arc<AtomicUsize>,
    limits: ProcessLimits,
) -> (DeterministicDriver, lenso_kernel::NativeApp) {
    start_process_app_with_engine(
        source,
        destination,
        source_calls,
        destination_calls,
        limits,
        false,
        false,
    )
}

fn start_process_app_with_engine(
    source: &Arc<Mutex<BTreeMap<String, String>>>,
    destination: &Arc<Mutex<BTreeMap<String, String>>>,
    source_calls: &Arc<AtomicUsize>,
    destination_calls: &Arc<AtomicUsize>,
    limits: ProcessLimits,
    generic_engine: bool,
    lifecycle_calls: bool,
) -> (DeterministicDriver, lenso_kernel::NativeApp) {
    start_process_app_with_configuration(
        source,
        destination,
        source_calls,
        destination_calls,
        limits,
        generic_engine,
        &json!({"lifecycle_calls": lifecycle_calls}),
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn start_process_app_with_configuration(
    source: &Arc<Mutex<BTreeMap<String, String>>>,
    destination: &Arc<Mutex<BTreeMap<String, String>>>,
    source_calls: &Arc<AtomicUsize>,
    destination_calls: &Arc<AtomicUsize>,
    limits: ProcessLimits,
    generic_engine: bool,
    configuration: &Value,
    host_essential: bool,
) -> (DeterministicDriver, lenso_kernel::NativeApp) {
    let executable = std::path::Path::new(env!("CARGO_BIN_EXE_lenso-process-v2-test-fixture"));
    let bytes = fs::read(executable).unwrap();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
    let artifact = ArtifactHandle::open(executable, &digest, bytes.len() as u64).unwrap();
    let artifacts = ArtifactCatalog::new()
        .with_artifact("sync", artifact)
        .unwrap();
    let native = NativePluginRegistry::new()
        .with_factory(StoreFactory {
            package: "test.source-store",
            state: source.clone(),
            calls: source_calls.clone(),
        })
        .with_factory(StoreFactory {
            package: "test.destination-store",
            state: destination.clone(),
            calls: destination_calls.clone(),
        })
        .with_factory(EmptyConsumerFactory);
    let adapters = ExecutionAdapterCatalog::new().with_adapter(native).unwrap();
    let adapters = if generic_engine {
        adapters
            .with_adapter(
                AuthoringProcessAdapter::new(
                    LANGUAGE_EXECUTION_CLASS,
                    RUNTIME_PROFILE_V2,
                    artifacts,
                )
                .with_codec(StoreCodec)
                .with_codec(SyncCodec)
                .with_limits(limits),
            )
            .unwrap()
    } else {
        adapters
            .with_adapter(
                ProcessAdapter::new(artifacts)
                    .with_codec(StoreCodec)
                    .with_codec(SyncCodec)
                    .with_limits(limits),
            )
            .unwrap()
    };
    let process_execution_class = if generic_engine {
        LANGUAGE_EXECUTION_CLASS
    } else {
        EXECUTION_CLASS
    };
    let plan = AppComposition::new(
        vec![
            PluginInstancePlan::new("source", "test.source-store").with_capability(
                CapabilityEndpointPlan::new(STORE_ID, DESCRIPTOR_VERSION, ["put", "read"]),
            ),
            PluginInstancePlan::new("destination", "test.destination-store").with_capability(
                CapabilityEndpointPlan::new(STORE_ID, DESCRIPTOR_VERSION, ["put", "read"]),
            ),
            PluginInstancePlan::new("sync", "test.process-v2")
                .with_configuration(configuration.to_string())
                .with_authoring(2, RUNTIME_PROFILE_V2)
                .with_entrypoint("plugin")
                .with_execution_class(ExecutionClassId::new(process_execution_class))
                .with_requirement(
                    CapabilityRequirementPlan::one(STORE_ID, DESCRIPTOR_VERSION)
                        .with_requirement_id("source"),
                )
                .with_requirement(
                    CapabilityRequirementPlan::one(STORE_ID, DESCRIPTOR_VERSION)
                        .with_requirement_id("destination"),
                )
                .with_capability(CapabilityEndpointPlan::new(
                    SYNC_ID,
                    DESCRIPTOR_VERSION,
                    ["sync"],
                )),
            PluginInstancePlan::new("consumer", "test.sync-consumer")
                .with_requirement(CapabilityRequirementPlan::one(SYNC_ID, DESCRIPTOR_VERSION)),
        ],
        vec![
            CapabilityBinding::new("sync", STORE_ID, DESCRIPTOR_VERSION, "source")
                .with_requirement_id("source"),
            CapabilityBinding::new("sync", STORE_ID, DESCRIPTOR_VERSION, "destination")
                .with_requirement_id("destination"),
            CapabilityBinding::new("consumer", SYNC_ID, DESCRIPTOR_VERSION, "sync"),
        ],
    )
    .resolve()
    .unwrap();
    let plan = apply_terminal_policy(plan, host_essential);
    let driver = DeterministicDriver::new();
    let app = driver
        .run(Kernel::start(plan, driver.clone(), adapters))
        .expect("the Process V2 App should activate");
    (driver, app)
}

fn apply_terminal_policy(
    plan: lenso_app_plan::ResolvedAppPlan,
    host_essential: bool,
) -> lenso_app_plan::ResolvedAppPlan {
    if !host_essential {
        return plan;
    }
    plan.with_terminal_policy(TerminalPolicy::HostEssential {
        roots: vec!["sync".to_owned()],
        closure: vec![
            "destination".to_owned(),
            "source".to_owned(),
            "sync".to_owned(),
        ],
    })
}
