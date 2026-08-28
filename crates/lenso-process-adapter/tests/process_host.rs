#![cfg(feature = "test-fixture")]

use std::{any::Any, fs};

use lenso_app_plan::{
    CapabilityEndpointPlan, ExecutionClassId, PluginInstancePlan, ResolvedAppPlan,
};
use lenso_kernel::{CancellationToken, ExecutionAdapter, InvocationContext, RuntimeFailure};
use lenso_process_adapter::{EXECUTION_CLASS, ProcessAdapter};
use lenso_runtime_codec::{ArtifactCatalog, ArtifactHandle, JsonCapabilityCodec};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

#[derive(Debug)]
struct EchoCodec;

impl JsonCapabilityCodec for EchoCodec {
    fn capability_id(&self) -> &'static str {
        "example.echo@1"
    }

    fn descriptor_version(&self) -> &'static str {
        "1.0.0"
    }

    fn request_operations(&self) -> &'static [&'static str] {
        &["echo"]
    }

    fn encode_request(&self, _: &str, request: &dyn Any) -> Result<Value, RuntimeFailure> {
        request
            .downcast_ref::<Value>()
            .cloned()
            .ok_or(RuntimeFailure::ProtocolViolation {
                capability: "example.echo@1",
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
fn real_process_descriptor_request_and_shutdown_cross_the_stdio_boundary() {
    let generation = process_generation();
    let outcome = futures::executor::block_on(generation.endpoints()[0].invoke(
        "echo",
        Box::new(json!({"message": "hello"})),
        InvocationContext::new(1, None, CancellationToken::new()),
    ))
    .unwrap()
    .unwrap();
    let value = outcome.downcast::<Value>().unwrap();

    assert_eq!(*value, json!({"message": "hello"}));
    drop(generation);
}

#[test]
fn cancellation_retires_a_blocked_process_without_replaying_the_request() {
    let generation = process_generation();
    let cancellation = CancellationToken::new();
    let cancel_after_admission = cancellation.clone();
    let invocation = generation.endpoints()[0].invoke(
        "echo",
        Box::new(json!({"sleep_ms": 5_000})),
        InvocationContext::new(2, None, cancellation),
    );
    let (result, ()) = futures::executor::block_on(futures::future::join(invocation, async move {
        cancel_after_admission.cancel();
    }));

    assert!(matches!(result, Err(RuntimeFailure::Cancelled { .. })));
    drop(generation);
}

fn process_generation() -> lenso_kernel::PreparedNativePlugin {
    let executable = std::path::Path::new(env!("CARGO_BIN_EXE_lenso-process-test-fixture"));
    let bytes = fs::read(executable).unwrap();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
    let artifact = ArtifactHandle::open(executable, &digest, bytes.len() as u64).unwrap();
    let artifacts = ArtifactCatalog::new()
        .with_artifact("plugin", artifact)
        .unwrap();
    let adapter = ProcessAdapter::new(artifacts).with_codec(EchoCodec);
    let plan = ResolvedAppPlan::new(
        vec![
            PluginInstancePlan::new("plugin", "example.process")
                .with_entrypoint("plugin")
                .with_execution_class(ExecutionClassId::new(EXECUTION_CLASS))
                .with_capability(CapabilityEndpointPlan::new(
                    "example.echo@1",
                    "1.0.0",
                    ["echo"],
                )),
        ],
        Vec::new(),
    );
    adapter.recreate(&plan, "plugin").unwrap()
}
