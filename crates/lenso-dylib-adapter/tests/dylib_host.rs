#![cfg(unix)]

use std::{any::Any, process::Command};

use lenso_app_plan::{
    CapabilityEndpointPlan, ExecutionClassId, PluginInstancePlan, ResolvedAppPlan,
};
use lenso_dylib_adapter::{DylibAdapter, EXECUTION_CLASS, ExplicitDigestTrust};
use lenso_kernel::{CancellationToken, ExecutionAdapter, InvocationContext, RuntimeFailure};
use lenso_runtime_codec::{ArtifactCatalog, ArtifactHandle, JsonCapabilityCodec};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug)]
struct EchoCodec;

impl JsonCapabilityCodec for EchoCodec {
    fn capability_id(&self) -> &'static str {
        "test.echo@1"
    }
    fn descriptor_version(&self) -> &'static str {
        "1.0.0"
    }
    fn request_operations(&self) -> &'static [&'static str] {
        &["echo", "fail"]
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

#[test]
fn versioned_c_abi_uses_host_owned_buffers_and_exact_trust() {
    let directory = tempfile::tempdir().unwrap();
    let compiled = directory.path().join("plugin-under-test");
    let mut command = Command::new("cc");
    #[cfg(target_os = "macos")]
    command.arg("-dynamiclib");
    #[cfg(not(target_os = "macos"))]
    command.args(["-shared", "-fPIC"]);
    let status = command
        .args(["-O2", "-fvisibility=hidden"])
        .arg("tests/fixtures/plugin.c")
        .arg("-o")
        .arg(&compiled)
        .status()
        .unwrap();
    assert!(status.success());
    let bytes = std::fs::read(&compiled).unwrap();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
    let content_path = directory
        .path()
        .join(digest.strip_prefix("sha256:").unwrap());
    std::fs::rename(&compiled, &content_path).unwrap();
    let artifact = ArtifactHandle::open(&content_path, &digest, bytes.len() as u64).unwrap();
    let stable_path = artifact.path().to_path_buf();
    let artifacts = ArtifactCatalog::new()
        .with_artifact("plugin", artifact)
        .unwrap();
    let adapter =
        DylibAdapter::new(artifacts, ExplicitDigestTrust::new([digest])).with_codec(EchoCodec);
    std::fs::write(&content_path, b"source drift after admission").unwrap();
    let plan = plan();
    let generation = adapter.recreate(&plan, "plugin").unwrap();
    drop(adapter);
    assert!(stable_path.exists());
    let endpoint = generation.endpoints()[0].clone();

    let context = InvocationContext::new(1, None, CancellationToken::new());
    let Ok(Ok(response)) =
        futures::executor::block_on(endpoint.invoke("echo", Box::new(7_u64), context))
    else {
        panic!("dylib echo did not succeed");
    };
    assert_eq!(*response.downcast::<u64>().unwrap(), 7);

    let context = InvocationContext::new(2, None, CancellationToken::new());
    let Ok(Err(error)) =
        futures::executor::block_on(endpoint.invoke("fail", Box::new(0_u64), context))
    else {
        panic!("dylib fail did not return a Domain Error");
    };
    assert_eq!(*error.downcast::<String>().unwrap(), "declared");

    drop(endpoint);
    drop(generation);
    assert!(!stable_path.exists());
}

fn plan() -> ResolvedAppPlan {
    ResolvedAppPlan::new(
        vec![
            PluginInstancePlan::new("plugin", "test.dylib")
                .with_entrypoint("lenso_plugin_v1")
                .with_execution_class(ExecutionClassId::new(EXECUTION_CLASS))
                .with_capability(CapabilityEndpointPlan::new(
                    "test.echo@1",
                    "1.0.0",
                    ["echo", "fail"],
                )),
        ],
        Vec::new(),
    )
}
