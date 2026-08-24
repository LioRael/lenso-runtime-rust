use std::{any::Any, time::Duration};

use lenso_app_plan::{
    CapabilityEndpointPlan, ExecutionClassId, ModuleInstancePlan, ResolvedAppPlan,
};
use lenso_kernel::{CancellationToken, ExecutionAdapter, InvocationContext, RuntimeFailure};
use lenso_quickjs_adapter::{EXECUTION_CLASS, QuickJsAdapter, QuickJsLimits};
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
        &["echo", "fail", "loop"]
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
fn bundled_esm_runs_without_ambient_host_apis_and_recreates_after_interrupt() {
    let source = br#"
        export async function invoke(operation, requestJson) {
          if (globalThis.Date !== undefined || Math.random !== undefined) throw new Error("ambient");
          if (operation === "loop") while (true) {}
          if (operation === "fail") return JSON.stringify({error: "declared"});
          return JSON.stringify({ok: JSON.parse(requestJson)});
        }
    "#;
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), source).unwrap();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(source)));
    let artifact = ArtifactHandle::open(file.path(), &digest, source.len() as u64).unwrap();
    let artifacts = ArtifactCatalog::new()
        .with_artifact("plugin", artifact)
        .unwrap();
    let adapter = QuickJsAdapter::new(artifacts)
        .with_codec(EchoCodec)
        .with_limits(QuickJsLimits {
            max_turn: Duration::from_millis(20),
            ..QuickJsLimits::default()
        });
    let plan = plan();

    let generation = adapter.recreate(&plan, "plugin").unwrap();
    let endpoint = generation.endpoints()[0].clone();
    let context = InvocationContext::new(1, None, CancellationToken::new());
    let Ok(Ok(success)) =
        futures::executor::block_on(endpoint.invoke("echo", Box::new(42_u64), context))
    else {
        panic!("echo request did not succeed");
    };
    assert_eq!(*success.downcast::<u64>().unwrap(), 42);

    let context = InvocationContext::new(2, None, CancellationToken::new());
    let Ok(Err(domain)) =
        futures::executor::block_on(endpoint.invoke("fail", Box::new(0_u64), context))
    else {
        panic!("fail request did not return a Domain Error");
    };
    assert_eq!(*domain.downcast::<String>().unwrap(), "declared");

    let context = InvocationContext::new(3, None, CancellationToken::new());
    let failure = futures::executor::block_on(endpoint.invoke("loop", Box::new(0_u64), context));
    assert!(matches!(failure, Err(RuntimeFailure::ModuleFailure { .. })));
    let recreated = adapter.recreate(&plan, "plugin").unwrap();
    let endpoint = recreated.endpoints()[0].clone();
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    let invocation = endpoint.invoke(
        "loop",
        Box::new(0_u64),
        InvocationContext::new(4, None, cancellation),
    );
    let (failure, ()) = futures::executor::block_on(async move {
        futures::join!(invocation, async move { cancel.cancel() })
    });
    assert!(matches!(
        failure,
        Err(RuntimeFailure::Cancelled { request_id: 4 })
    ));
    assert!(adapter.recreate(&plan, "plugin").is_ok());
}

fn plan() -> ResolvedAppPlan {
    ResolvedAppPlan::new(
        vec![
            ModuleInstancePlan::new("plugin", "test.quickjs")
                .with_entrypoint("plugin.mjs")
                .with_execution_class(ExecutionClassId::new(EXECUTION_CLASS))
                .with_capability(CapabilityEndpointPlan::new(
                    "test.echo@1",
                    "1.0.0",
                    ["echo", "fail", "loop"],
                )),
        ],
        Vec::new(),
    )
}
