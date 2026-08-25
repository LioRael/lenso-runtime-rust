use std::{any::Any, time::Duration};

use lenso_app_plan::{
    CapabilityEndpointPlan, ExecutionClassId, ModuleInstancePlan, ResolvedAppPlan,
};
use lenso_kernel::{
    CancellationToken, ExecutionAdapter, InvocationContext, NativeStreamItem, RuntimeFailure,
};
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
fn bundled_esm_stream_preserves_messages_half_close_terminal_and_open_error() {
    let source = br#"
        const sessions = new Map();
        let nextId = 1;
        export function describe() {
          return JSON.stringify({
            abi: "lenso.json-interactions@1",
            capabilities: [{
              capability_id: "test.chat@1",
              descriptor_version: "1.0.0",
              request_operations: [],
              stream_operations: ["chat"]
            }]
          });
        }
        export function invoke() { return JSON.stringify({ok: null}); }
        export function streamOpen(capability, operation, requestJson) {
          if (capability !== "test.chat@1" || operation !== "chat") throw new Error("identity");
          if (JSON.parse(requestJson) === 0) return JSON.stringify({error: "rejected"});
          const id = nextId++; sessions.set(id, {messages: [], closed: false, terminal: false});
          return JSON.stringify({ok: id});
        }
        export function streamSend(id, messageJson) {
          sessions.get(id).messages.push(JSON.parse(messageJson));
          return JSON.stringify({ok: null});
        }
        export function streamReceive(id) {
          const session = sessions.get(id);
          if (session.messages.length) return JSON.stringify({ok: {kind: "message", value: session.messages.shift()}});
          if (session.closed) { session.closed = false; return JSON.stringify({ok: {kind: "peer-half-closed"}}); }
          session.terminal = true; return JSON.stringify({ok: {kind: "terminal-success"}});
        }
        export function streamCloseSend(id) { sessions.get(id).closed = true; return JSON.stringify({ok: null}); }
        export function streamCancel(id) { sessions.delete(id); return JSON.stringify({ok: null}); }
    "#;
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), source).unwrap();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(source)));
    let artifact = ArtifactHandle::open(file.path(), &digest, source.len() as u64).unwrap();
    let adapter = QuickJsAdapter::new(
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
fn bundled_esm_runs_without_ambient_host_apis_and_recreates_after_interrupt() {
    let source = br#"
        export function describe() {
          return JSON.stringify({
            abi: "lenso.json-request@1",
            capabilities: [{
              capability_id: "test.echo@1",
              descriptor_version: "1.0.0",
              request_operations: ["echo", "fail", "loop"]
            }]
          });
        }
        export async function invoke(capability, operation, requestJson) {
          if (globalThis.Date !== undefined || Math.random !== undefined) throw new Error("ambient");
          if (capability !== "test.echo@1") throw new Error("capability");
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

#[test]
fn readiness_rejects_descriptor_drift_and_duplicate_codecs() {
    let source = br#"
        export function describe() {
          return JSON.stringify({abi: "lenso.json-request@1", capabilities: []});
        }
        export function invoke() { return JSON.stringify({ok: null}); }
    "#;
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), source).unwrap();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(source)));
    let artifact = ArtifactHandle::open(file.path(), &digest, source.len() as u64).unwrap();
    let artifacts = ArtifactCatalog::new()
        .with_artifact("plugin", artifact)
        .unwrap();
    let mismatch = QuickJsAdapter::new(artifacts.clone()).with_codec(EchoCodec);
    assert!(mismatch.recreate(&plan(), "plugin").is_err());

    let duplicate = QuickJsAdapter::new(artifacts)
        .with_codec(EchoCodec)
        .with_codec(EchoCodec);
    assert!(duplicate.recreate(&plan(), "plugin").is_err());
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

fn stream_plan() -> ResolvedAppPlan {
    ResolvedAppPlan::new(
        vec![
            ModuleInstancePlan::new("plugin", "test.quickjs")
                .with_entrypoint("plugin.mjs")
                .with_execution_class(ExecutionClassId::new(EXECUTION_CLASS))
                .with_capability(
                    CapabilityEndpointPlan::new("test.chat@1", "1.0.0", ["chat"])
                        .with_stream_operation("chat"),
                ),
        ],
        Vec::new(),
    )
}
