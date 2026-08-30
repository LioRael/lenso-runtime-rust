#![cfg(feature = "test-fixture")]

use std::{
    any::Any,
    fs,
    future::Future as _,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    thread,
    time::Duration,
};

use futures::task::{ArcWake, waker};
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
    let generation = process_generation(std::path::Path::new(env!(
        "CARGO_BIN_EXE_lenso-process-test-fixture"
    )));
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
    let generation = process_generation(std::path::Path::new(env!(
        "CARGO_BIN_EXE_lenso-process-test-fixture"
    )));
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

#[test]
fn abandoning_an_admitted_future_retires_the_process_generation() {
    let generation = process_generation(std::path::Path::new(env!(
        "CARGO_BIN_EXE_lenso-process-test-fixture"
    )));
    let mut invocation = Box::pin(generation.endpoints()[0].invoke(
        "echo",
        Box::new(json!({"sleep_ms": 5_000})),
        InvocationContext::new(20, None, CancellationToken::new()),
    ));
    futures::executor::block_on(async {
        assert!(futures::poll!(&mut invocation).is_pending());
    });
    drop(invocation);

    let result = futures::executor::block_on(generation.endpoints()[0].invoke(
        "echo",
        Box::new(json!({"message": "after abandonment"})),
        InvocationContext::new(21, None, CancellationToken::new()),
    ));
    assert!(matches!(result, Err(RuntimeFailure::PluginFailure { .. })));
}

#[derive(Debug, Default)]
struct ResponseWake(AtomicBool);

impl ArcWake for ResponseWake {
    fn wake_by_ref(wake: &Arc<Self>) {
        wake.0.store(true, Ordering::Release);
    }
}

#[test]
fn dropping_an_already_settled_response_keeps_the_process_generation_available() {
    let generation = process_generation(std::path::Path::new(env!(
        "CARGO_BIN_EXE_lenso-process-test-fixture"
    )));
    let mut invocation = Box::pin(generation.endpoints()[0].invoke(
        "echo",
        Box::new(json!({"message": "settled"})),
        InvocationContext::new(22, None, CancellationToken::new()),
    ));
    let response_wake = Arc::new(ResponseWake::default());
    let response_waker = waker(Arc::clone(&response_wake));
    let mut context = Context::from_waker(&response_waker);
    assert!(matches!(
        invocation.as_mut().poll(&mut context),
        Poll::Pending
    ));
    for _ in 0..100 {
        if response_wake.0.load(Ordering::Acquire) {
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(
        response_wake.0.load(Ordering::Acquire),
        "the Process response should settle without re-polling the guest future"
    );
    drop(invocation);

    let outcome = futures::executor::block_on(generation.endpoints()[0].invoke(
        "echo",
        Box::new(json!({"message": "still healthy"})),
        InvocationContext::new(23, None, CancellationToken::new()),
    ))
    .unwrap()
    .unwrap();
    assert_eq!(
        *outcome.downcast::<Value>().unwrap(),
        json!({"message": "still healthy"})
    );
}

#[test]
fn non_ascii_process_failure_is_truncated_on_a_utf8_boundary() {
    let generation = process_generation(std::path::Path::new(env!(
        "CARGO_BIN_EXE_lenso-process-test-fixture"
    )));
    let error = futures::executor::block_on(generation.endpoints()[0].invoke(
        "echo",
        Box::new(json!({"unicode_failure": true})),
        InvocationContext::new(24, None, CancellationToken::new()),
    ))
    .unwrap_err();

    let RuntimeFailure::PluginFailure { detail } = error else {
        panic!("expected the fixture's PluginFailure");
    };
    assert_eq!(detail.len(), 510);
    assert_eq!(detail.chars().count(), 170);
    assert!(detail.chars().all(|character| character == '界'));
}

#[test]
fn relative_artifact_path_starts_after_the_child_working_directory_changes() {
    let current = std::env::current_dir().unwrap();
    let directory = tempfile::Builder::new()
        .prefix("lenso-process-relative-")
        .tempdir_in(&current)
        .unwrap();
    let executable = directory.path().join("plugin");
    fs::copy(
        env!("CARGO_BIN_EXE_lenso-process-test-fixture"),
        &executable,
    )
    .unwrap();
    let relative = executable.strip_prefix(&current).unwrap();

    let generation = process_generation(relative);

    drop(generation);
}

#[test]
fn explicit_host_staging_root_executes_the_private_artifact_copy() {
    let executable = std::path::Path::new(env!("CARGO_BIN_EXE_lenso-process-test-fixture"));
    let bytes = fs::read(executable).unwrap();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
    let staging = tempfile::tempdir().unwrap();
    let artifact = ArtifactHandle::open_with_staging_root(
        executable,
        &digest,
        bytes.len() as u64,
        staging.path(),
    )
    .unwrap();
    assert!(artifact.path().starts_with(staging.path()));

    let generation = process_generation_from_artifact(artifact);

    drop(generation);
}

fn process_generation(executable: &std::path::Path) -> lenso_kernel::PreparedNativePlugin {
    let bytes = fs::read(executable).unwrap();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
    let artifact = ArtifactHandle::open(executable, &digest, bytes.len() as u64).unwrap();
    process_generation_from_artifact(artifact)
}

fn process_generation_from_artifact(
    artifact: ArtifactHandle,
) -> lenso_kernel::PreparedNativePlugin {
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
