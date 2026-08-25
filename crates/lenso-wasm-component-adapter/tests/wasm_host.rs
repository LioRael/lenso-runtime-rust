use std::{any::Any, process::Command, time::Duration};

use futures::FutureExt;
use lenso_app_plan::{
    AppComposition, CapabilityBinding, CapabilityEndpointPlan, CapabilityRequirementPlan,
    ExecutionClassId, ModuleInstancePlan, ResolvedAppPlan,
};
use lenso_kernel::{
    CancellationToken, DeterministicDriver, ExecutionAdapter, ExecutionAdapterCatalog,
    InvocationContext, Kernel, ModuleDependencyHandle, NativeStreamItem, RequestCapability,
    RuntimeFailure,
};
use lenso_runtime_codec::{
    ArtifactCatalog, ArtifactHandle, JsonCapabilityCodec, JsonHostRequestFuture,
    JsonInvocationOutcome,
};
use lenso_runtime_conformance::{
    ConformanceExecutionAdapter, ConformanceModule, ConformanceModuleFactory, PROBE_CAPABILITY_ID,
    PROBE_DESCRIPTOR_VERSION, PROBE_OPERATION, PROBE_PROVIDER_PACKAGE_ID, Probe, ProbeError,
    ProbeProviderFactory, ProbeRequest,
};
use lenso_wasm_component_adapter::{EXECUTION_CLASS, WasmComponentAdapter, WasmComponentLimits};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug)]
struct EchoCodec;

#[derive(Debug)]
struct EchoCapability;

impl RequestCapability for EchoCapability {
    type Request = u64;
    type Response = u64;
    type DomainError = String;

    const ID: &'static str = "test.echo@1";
    const DESCRIPTOR_VERSION: &'static str = "1.0.0";
}

#[derive(Debug)]
struct ProbeCodec;

impl JsonCapabilityCodec for ProbeCodec {
    fn capability_id(&self) -> &'static str {
        PROBE_CAPABILITY_ID
    }

    fn descriptor_version(&self) -> &'static str {
        PROBE_DESCRIPTOR_VERSION
    }

    fn request_operations(&self) -> &'static [&'static str] {
        &[PROBE_OPERATION]
    }

    fn encode_request(&self, _: &str, _: &dyn Any) -> Result<Value, RuntimeFailure> {
        Err(RuntimeFailure::ProtocolViolation {
            capability: PROBE_CAPABILITY_ID,
        })
    }

    fn decode_response(&self, _: &str, _: Value) -> Result<Box<dyn Any>, RuntimeFailure> {
        Err(RuntimeFailure::ProtocolViolation {
            capability: PROBE_CAPABILITY_ID,
        })
    }

    fn decode_domain_error(&self, _: &str, _: Value) -> Result<Box<dyn Any>, RuntimeFailure> {
        Err(RuntimeFailure::ProtocolViolation {
            capability: PROBE_CAPABILITY_ID,
        })
    }

    fn invoke_host_request(
        &self,
        dependency: ModuleDependencyHandle,
        operation: String,
        request: Value,
        context: InvocationContext,
    ) -> JsonHostRequestFuture {
        Box::pin(async move {
            if operation != PROBE_OPERATION {
                return Err(RuntimeFailure::UnknownOperation {
                    capability: PROBE_CAPABILITY_ID,
                    operation,
                });
            }
            let value = request
                .get("value")
                .and_then(Value::as_str)
                .ok_or(RuntimeFailure::ProtocolViolation {
                    capability: PROBE_CAPABILITY_ID,
                })?
                .to_owned();
            match dependency
                .typed::<Probe>()?
                .invoke_with_context(PROBE_OPERATION, context, ProbeRequest { value })
                .await?
            {
                Ok(response) => Ok(JsonInvocationOutcome::Success(
                    serde_json::json!({ "value": response.value }),
                )),
                Err(ProbeError::EmptyValue) => Ok(JsonInvocationOutcome::DomainError(
                    serde_json::json!({ "kind": "empty_value" }),
                )),
            }
        })
    }
}

#[derive(Debug)]
struct EmptyConsumerFactory;

impl ConformanceModuleFactory for EmptyConsumerFactory {
    fn package_id(&self) -> &'static str {
        "test.echo-consumer"
    }

    fn instantiate(&self, _: &ModuleInstancePlan) -> Result<ConformanceModule, RuntimeFailure> {
        Ok(ConformanceModule::default())
    }
}

impl JsonCapabilityCodec for EchoCodec {
    fn capability_id(&self) -> &'static str {
        "test.echo@1"
    }
    fn descriptor_version(&self) -> &'static str {
        "1.0.0"
    }
    fn request_operations(&self) -> &'static [&'static str] {
        &["echo", "fail", "trap", "loop"]
    }
    fn encode_request(&self, _operation: &str, request: &dyn Any) -> Result<Value, RuntimeFailure> {
        request
            .downcast_ref::<u64>()
            .copied()
            .map(Value::from)
            .ok_or(RuntimeFailure::ProtocolViolation {
                capability: self.capability_id(),
            })
    }
    fn decode_response(
        &self,
        _operation: &str,
        value: Value,
    ) -> Result<Box<dyn Any>, RuntimeFailure> {
        value
            .as_u64()
            .map(|value| Box::new(value) as Box<dyn Any>)
            .ok_or(RuntimeFailure::ProtocolViolation {
                capability: self.capability_id(),
            })
    }
    fn decode_domain_error(
        &self,
        _operation: &str,
        value: Value,
    ) -> Result<Box<dyn Any>, RuntimeFailure> {
        value
            .as_str()
            .map(|value| Box::new(value.to_owned()) as Box<dyn Any>)
            .ok_or(RuntimeFailure::ProtocolViolation {
                capability: self.capability_id(),
            })
    }
}

#[derive(Debug)]
struct NarrowCodec;

impl JsonCapabilityCodec for NarrowCodec {
    fn capability_id(&self) -> &'static str {
        "test.echo@1"
    }
    fn descriptor_version(&self) -> &'static str {
        "1.0.0"
    }
    fn request_operations(&self) -> &'static [&'static str] {
        &["echo"]
    }
    fn encode_request(&self, operation: &str, request: &dyn Any) -> Result<Value, RuntimeFailure> {
        EchoCodec.encode_request(operation, request)
    }
    fn decode_response(
        &self,
        operation: &str,
        value: Value,
    ) -> Result<Box<dyn Any>, RuntimeFailure> {
        EchoCodec.decode_response(operation, value)
    }
    fn decode_domain_error(
        &self,
        operation: &str,
        value: Value,
    ) -> Result<Box<dyn Any>, RuntimeFailure> {
        EchoCodec.decode_domain_error(operation, value)
    }
}

#[derive(Debug)]
struct ChatCodec;

impl JsonCapabilityCodec for ChatCodec {
    fn capability_id(&self) -> &'static str {
        "test.chat@1"
    }
    fn descriptor_version(&self) -> &'static str {
        "1.0.0"
    }
    fn request_operations(&self) -> &'static [&'static str] {
        &[]
    }
    fn stream_operations(&self) -> &'static [&'static str] {
        &["chat"]
    }
    fn encode_request(&self, operation: &str, _: &dyn Any) -> Result<Value, RuntimeFailure> {
        Err(RuntimeFailure::UnknownOperation {
            capability: self.capability_id(),
            operation: operation.to_owned(),
        })
    }
    fn decode_response(&self, operation: &str, _: Value) -> Result<Box<dyn Any>, RuntimeFailure> {
        Err(RuntimeFailure::UnknownOperation {
            capability: self.capability_id(),
            operation: operation.to_owned(),
        })
    }
    fn decode_domain_error(
        &self,
        operation: &str,
        _: Value,
    ) -> Result<Box<dyn Any>, RuntimeFailure> {
        Err(RuntimeFailure::UnknownOperation {
            capability: self.capability_id(),
            operation: operation.to_owned(),
        })
    }
    fn encode_stream_open(&self, _: &str, request: &dyn Any) -> Result<Value, RuntimeFailure> {
        request
            .downcast_ref::<u64>()
            .copied()
            .map(Value::from)
            .ok_or(RuntimeFailure::ProtocolViolation {
                capability: self.capability_id(),
            })
    }
    fn encode_stream_message(&self, _: &str, message: &dyn Any) -> Result<Value, RuntimeFailure> {
        message
            .downcast_ref::<String>()
            .cloned()
            .map(Value::from)
            .ok_or(RuntimeFailure::ProtocolViolation {
                capability: self.capability_id(),
            })
    }
    fn decode_stream_message(&self, _: &str, value: Value) -> Result<Box<dyn Any>, RuntimeFailure> {
        value
            .as_str()
            .map(|value| Box::new(value.to_owned()) as Box<dyn Any>)
            .ok_or(RuntimeFailure::ProtocolViolation {
                capability: self.capability_id(),
            })
    }
    fn decode_stream_domain_error(
        &self,
        _: &str,
        value: Value,
    ) -> Result<Box<dyn Any>, RuntimeFailure> {
        value
            .as_str()
            .map(|value| Box::new(value.to_owned()) as Box<dyn Any>)
            .ok_or(RuntimeFailure::ProtocolViolation {
                capability: self.capability_id(),
            })
    }
}

#[test]
fn real_component_import_invokes_only_the_plan_bound_host_capability() {
    let fixture_target = tempfile::tempdir().unwrap();
    let status = Command::new(env!("CARGO"))
        .args([
            "build",
            "--locked",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
            "--manifest-path",
            "tests/fixtures/rust-host-import-guest/Cargo.toml",
            "--target-dir",
        ])
        .arg(fixture_target.path())
        .status()
        .unwrap();
    assert!(status.success());
    let core = std::fs::read(
        fixture_target
            .path()
            .join("wasm32-unknown-unknown/release/lenso_wasm_host_import_test_guest.wasm"),
    )
    .unwrap();
    let component = wit_component::ComponentEncoder::default()
        .module(&core)
        .unwrap()
        .validate(true)
        .encode()
        .unwrap();
    let artifact_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(artifact_file.path(), &component).unwrap();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&component)));
    let artifact =
        ArtifactHandle::open(artifact_file.path(), &digest, component.len() as u64).unwrap();
    let wasm = WasmComponentAdapter::new(
        ArtifactCatalog::new()
            .with_artifact("plugin", artifact)
            .unwrap(),
    )
    .with_codec(NarrowCodec)
    .with_codec(ProbeCodec);
    let adapters = ExecutionAdapterCatalog::new()
        .with_adapter(
            ConformanceExecutionAdapter::new()
                .with_factory(ProbeProviderFactory)
                .with_factory(EmptyConsumerFactory),
        )
        .unwrap()
        .with_adapter(wasm)
        .unwrap();
    let driver = DeterministicDriver::new();
    let app = driver
        .run(Kernel::start(
            wasm_guest_import_plan(),
            driver.clone(),
            adapters,
        ))
        .expect("the Wasm Guest Import App should activate");
    let result = driver
        .run(
            app.handle::<EchoCapability>("consumer")
                .unwrap()
                .invoke("echo", 7),
        )
        .expect("the Wasm guest should reach the host")
        .expect("the host should not return a Domain Error");
    assert_eq!(result, 7);
}

#[test]
fn real_component_stream_preserves_messages_half_close_terminal_and_open_error() {
    let fixture_target = tempfile::tempdir().unwrap();
    let status = Command::new(env!("CARGO"))
        .args([
            "build",
            "--locked",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
            "--manifest-path",
            "tests/fixtures/rust-stream-guest/Cargo.toml",
            "--target-dir",
        ])
        .arg(fixture_target.path())
        .status()
        .unwrap();
    assert!(status.success());
    let core = std::fs::read(
        fixture_target
            .path()
            .join("wasm32-unknown-unknown/release/lenso_wasm_stream_test_guest.wasm"),
    )
    .unwrap();
    let component = wit_component::ComponentEncoder::default()
        .module(&core)
        .unwrap()
        .validate(true)
        .encode()
        .unwrap();
    let artifact_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(artifact_file.path(), &component).unwrap();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&component)));
    let artifact =
        ArtifactHandle::open(artifact_file.path(), &digest, component.len() as u64).unwrap();
    let adapter = WasmComponentAdapter::new(
        ArtifactCatalog::new()
            .with_artifact("plugin", artifact)
            .unwrap(),
    )
    .with_codec(ChatCodec);
    let plan = stream_plan();
    let generation = adapter.recreate(&plan, "plugin").unwrap();
    assert!(generation.endpoints().is_empty());
    let endpoint = generation.stream_endpoints()[0].clone();
    let context = InvocationContext::new(10, None, CancellationToken::new());
    let Ok(Ok(session)) =
        futures::executor::block_on(endpoint.open("chat", Box::new(7_u64), context))
    else {
        panic!("stream did not open")
    };
    futures::executor::block_on(session.send(Box::new("hello".to_owned()))).unwrap();
    let NativeStreamItem::Message(message) =
        futures::executor::block_on(session.receive()).unwrap()
    else {
        panic!("message missing")
    };
    assert_eq!(*message.downcast::<String>().unwrap(), "hello");
    futures::executor::block_on(session.close_send()).unwrap();
    assert!(matches!(
        futures::executor::block_on(session.receive()).unwrap(),
        NativeStreamItem::PeerHalfClosed
    ));
    assert!(matches!(
        futures::executor::block_on(session.receive()).unwrap(),
        NativeStreamItem::Terminal(Ok(()))
    ));

    let context = InvocationContext::new(11, None, CancellationToken::new());
    let Ok(Err(error)) =
        futures::executor::block_on(endpoint.open("chat", Box::new(0_u64), context))
    else {
        panic!("open error missing")
    };
    assert_eq!(*error.downcast::<String>().unwrap(), "rejected");
}

#[test]
fn real_component_runs_without_wasi_and_retires_on_trap_or_cancellation() {
    let fixture_target = tempfile::tempdir().unwrap();
    let status = Command::new(env!("CARGO"))
        .args([
            "build",
            "--locked",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
            "--manifest-path",
            "tests/fixtures/rust-guest/Cargo.toml",
            "--target-dir",
        ])
        .arg(fixture_target.path())
        .status()
        .unwrap();
    assert!(status.success());
    let core = std::fs::read(
        fixture_target
            .path()
            .join("wasm32-unknown-unknown/release/lenso_wasm_test_guest.wasm"),
    )
    .unwrap();
    let component = wit_component::ComponentEncoder::default()
        .module(&core)
        .unwrap()
        .validate(true)
        .encode()
        .unwrap();
    let artifact_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(artifact_file.path(), &component).unwrap();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&component)));
    let artifact =
        ArtifactHandle::open(artifact_file.path(), &digest, component.len() as u64).unwrap();
    let artifacts = ArtifactCatalog::new()
        .with_artifact("plugin", artifact)
        .unwrap();
    let mismatch = WasmComponentAdapter::new(artifacts.clone()).with_codec(NarrowCodec);
    assert!(mismatch.recreate(&narrow_plan(), "plugin").is_err());
    let duplicate = WasmComponentAdapter::new(artifacts.clone())
        .with_codec(EchoCodec)
        .with_codec(EchoCodec);
    assert!(duplicate.recreate(&plan(), "plugin").is_err());

    let adapter = WasmComponentAdapter::new(artifacts)
        .with_codec(EchoCodec)
        .with_limits(WasmComponentLimits {
            max_turn: Duration::from_millis(50),
            ..WasmComponentLimits::default()
        });
    let plan = plan();
    let generation = adapter.recreate(&plan, "plugin").unwrap();
    let endpoint = generation.endpoints()[0].clone();

    let context = InvocationContext::new(1, None, CancellationToken::new());
    let Ok(Ok(response)) =
        futures::executor::block_on(endpoint.invoke("echo", Box::new(9_u64), context))
    else {
        panic!("Component echo did not succeed");
    };
    assert_eq!(*response.downcast::<u64>().unwrap(), 9);

    let context = InvocationContext::new(2, None, CancellationToken::new());
    let Ok(Err(error)) =
        futures::executor::block_on(endpoint.invoke("fail", Box::new(0_u64), context))
    else {
        panic!("Component fail did not return a Domain Error");
    };
    assert_eq!(*error.downcast::<String>().unwrap(), "declared");

    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    let invocation = endpoint
        .invoke(
            "loop",
            Box::new(0_u64),
            InvocationContext::new(3, None, cancellation),
        )
        .fuse();
    let cancellation = async move {
        futures::future::ready(()).await;
        cancel.cancel();
    };
    let (failure, ()) =
        futures::executor::block_on(async { futures::join!(invocation, cancellation) });
    assert!(matches!(
        failure,
        Err(RuntimeFailure::Cancelled { request_id: 3 })
    ));
    assert!(adapter.recreate(&plan, "plugin").is_ok());
}

fn narrow_plan() -> ResolvedAppPlan {
    ResolvedAppPlan::new(
        vec![
            ModuleInstancePlan::new("plugin", "test.component")
                .with_entrypoint("plugin")
                .with_execution_class(ExecutionClassId::new(EXECUTION_CLASS))
                .with_capability(CapabilityEndpointPlan::new(
                    "test.echo@1",
                    "1.0.0",
                    ["echo"],
                )),
        ],
        Vec::new(),
    )
}

fn plan() -> ResolvedAppPlan {
    ResolvedAppPlan::new(
        vec![
            ModuleInstancePlan::new("plugin", "test.component")
                .with_entrypoint("plugin")
                .with_execution_class(ExecutionClassId::new(EXECUTION_CLASS))
                .with_capability(CapabilityEndpointPlan::new(
                    "test.echo@1",
                    "1.0.0",
                    ["echo", "fail", "trap", "loop"],
                )),
        ],
        Vec::new(),
    )
}

fn stream_plan() -> ResolvedAppPlan {
    ResolvedAppPlan::new(
        vec![
            ModuleInstancePlan::new("plugin", "test.component")
                .with_entrypoint("plugin")
                .with_execution_class(ExecutionClassId::new(EXECUTION_CLASS))
                .with_capability(
                    CapabilityEndpointPlan::new("test.chat@1", "1.0.0", ["chat"])
                        .with_stream_operation("chat"),
                ),
        ],
        Vec::new(),
    )
}

fn wasm_guest_import_plan() -> ResolvedAppPlan {
    AppComposition::new(
        vec![
            ModuleInstancePlan::new("provider", PROBE_PROVIDER_PACKAGE_ID).with_capability(
                CapabilityEndpointPlan::new(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                    [PROBE_OPERATION],
                ),
            ),
            ModuleInstancePlan::new("plugin", "test.component")
                .with_entrypoint("plugin")
                .with_execution_class(ExecutionClassId::new(EXECUTION_CLASS))
                .with_requirement(CapabilityRequirementPlan::one(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                ))
                .with_capability(CapabilityEndpointPlan::new(
                    EchoCapability::ID,
                    EchoCapability::DESCRIPTOR_VERSION,
                    ["echo"],
                )),
            ModuleInstancePlan::new("consumer", "test.echo-consumer").with_requirement(
                CapabilityRequirementPlan::one(
                    EchoCapability::ID,
                    EchoCapability::DESCRIPTOR_VERSION,
                ),
            ),
        ],
        vec![
            CapabilityBinding::new(
                "plugin",
                PROBE_CAPABILITY_ID,
                PROBE_DESCRIPTOR_VERSION,
                "provider",
            ),
            CapabilityBinding::new(
                "consumer",
                EchoCapability::ID,
                EchoCapability::DESCRIPTOR_VERSION,
                "plugin",
            ),
        ],
    )
    .resolve()
    .expect("the Wasm Guest Import fixture should resolve")
}
