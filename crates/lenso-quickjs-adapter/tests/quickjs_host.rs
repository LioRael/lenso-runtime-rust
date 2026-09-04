use std::{any::Any, time::Duration};

use lenso_app_plan::{
    AppComposition, CapabilityBinding, CapabilityEndpointPlan, CapabilityRequirementPlan,
    ExecutionClassId, PluginInstancePlan, ResolvedAppPlan,
};
use lenso_kernel::{
    CancellationToken, DeterministicDriver, ExecutionAdapter, ExecutionAdapterCatalog,
    InvocationContext, Kernel, NativeStreamItem, PluginDependencyHandle,
    PluginStreamDependencyHandle, RequestCapability, RuntimeFailure,
};
use lenso_quickjs_adapter::{EXECUTION_CLASS, QuickJsAdapter, QuickJsLimits};
use lenso_runtime_codec::{
    ArtifactCatalog, ArtifactHandle, JsonCapabilityCodec, JsonHostRequestFuture,
    JsonHostStreamOpenFuture, JsonInvocationOutcome, json_host_stream,
};
use lenso_runtime_conformance::{
    ConformanceExecutionAdapter, ConformancePlugin, ConformancePluginFactory, PROBE_CAPABILITY_ID,
    PROBE_DESCRIPTOR_VERSION, PROBE_OPERATION, PROBE_PROVIDER_PACKAGE_ID, Probe, ProbeError,
    ProbeProviderFactory, ProbeRequest, STREAM_PROBE_CAPABILITY_ID,
    STREAM_PROBE_DESCRIPTOR_VERSION, STREAM_PROBE_OPERATION, STREAM_PROBE_PROVIDER_PACKAGE_ID,
    StreamProbe, StreamProbeError, StreamProbeMessage, StreamProbeOpen, StreamProbeProviderFactory,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug)]
struct EchoCodec;

#[derive(Debug)]
struct Bridge;

impl RequestCapability for Bridge {
    type Request = String;
    type Response = String;
    type DomainError = String;

    const ID: &'static str = "test.bridge@1";
    const DESCRIPTOR_VERSION: &'static str = "1.0.0";
}

#[derive(Debug)]
struct BridgeCodec;

impl JsonCapabilityCodec for BridgeCodec {
    fn capability_id(&self) -> &'static str {
        Bridge::ID
    }

    fn descriptor_version(&self) -> &'static str {
        Bridge::DESCRIPTOR_VERSION
    }

    fn request_operations(&self) -> &'static [&'static str] {
        &["bridge"]
    }

    fn encode_request(&self, _: &str, request: &dyn Any) -> Result<Value, RuntimeFailure> {
        request
            .downcast_ref::<String>()
            .cloned()
            .map(Value::from)
            .ok_or(RuntimeFailure::ProtocolViolation {
                capability: Bridge::ID,
            })
    }

    fn decode_response(&self, _: &str, value: Value) -> Result<Box<dyn Any>, RuntimeFailure> {
        value
            .as_str()
            .map(|value| Box::new(value.to_owned()) as Box<dyn Any>)
            .ok_or(RuntimeFailure::ProtocolViolation {
                capability: Bridge::ID,
            })
    }

    fn decode_domain_error(&self, _: &str, value: Value) -> Result<Box<dyn Any>, RuntimeFailure> {
        value
            .as_str()
            .map(|value| Box::new(value.to_owned()) as Box<dyn Any>)
            .ok_or(RuntimeFailure::ProtocolViolation {
                capability: Bridge::ID,
            })
    }
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
        dependency: PluginDependencyHandle,
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
struct StreamProbeCodec;

impl JsonCapabilityCodec for StreamProbeCodec {
    fn capability_id(&self) -> &'static str {
        STREAM_PROBE_CAPABILITY_ID
    }

    fn descriptor_version(&self) -> &'static str {
        STREAM_PROBE_DESCRIPTOR_VERSION
    }

    fn request_operations(&self) -> &'static [&'static str] {
        &[]
    }

    fn stream_operations(&self) -> &'static [&'static str] {
        &[STREAM_PROBE_OPERATION]
    }

    fn encode_request(&self, operation: &str, _: &dyn Any) -> Result<Value, RuntimeFailure> {
        Err(RuntimeFailure::UnknownOperation {
            capability: STREAM_PROBE_CAPABILITY_ID,
            operation: operation.to_owned(),
        })
    }

    fn decode_response(&self, operation: &str, _: Value) -> Result<Box<dyn Any>, RuntimeFailure> {
        Err(RuntimeFailure::UnknownOperation {
            capability: STREAM_PROBE_CAPABILITY_ID,
            operation: operation.to_owned(),
        })
    }

    fn decode_domain_error(
        &self,
        operation: &str,
        _: Value,
    ) -> Result<Box<dyn Any>, RuntimeFailure> {
        Err(RuntimeFailure::UnknownOperation {
            capability: STREAM_PROBE_CAPABILITY_ID,
            operation: operation.to_owned(),
        })
    }

    fn open_host_stream(
        &self,
        dependency: PluginStreamDependencyHandle,
        operation: String,
        request: Value,
        context: InvocationContext,
    ) -> JsonHostStreamOpenFuture {
        Box::pin(async move {
            if operation != STREAM_PROBE_OPERATION {
                return Err(RuntimeFailure::UnknownOperation {
                    capability: STREAM_PROBE_CAPABILITY_ID,
                    operation,
                });
            }
            let value = request
                .get("value")
                .and_then(Value::as_str)
                .ok_or(RuntimeFailure::ProtocolViolation {
                    capability: STREAM_PROBE_CAPABILITY_ID,
                })?
                .to_owned();
            match dependency
                .typed::<StreamProbe>()?
                .open_with_context(STREAM_PROBE_OPERATION, context, StreamProbeOpen { value })
                .await?
            {
                Ok(stream) => Ok(Ok(json_host_stream(
                    stream,
                    |value| {
                        Ok(StreamProbeMessage {
                            sequence: value.get("sequence").and_then(Value::as_u64).ok_or(
                                RuntimeFailure::ProtocolViolation {
                                    capability: STREAM_PROBE_CAPABILITY_ID,
                                },
                            )?,
                            value: value
                                .get("value")
                                .and_then(Value::as_str)
                                .ok_or(RuntimeFailure::ProtocolViolation {
                                    capability: STREAM_PROBE_CAPABILITY_ID,
                                })?
                                .to_owned(),
                        })
                    },
                    |message| {
                        Ok(serde_json::json!({
                            "sequence": message.sequence,
                            "value": message.value,
                        }))
                    },
                    |StreamProbeError::Rejected| Ok(serde_json::json!({ "kind": "rejected" })),
                ))),
                Err(StreamProbeError::Rejected) => {
                    Ok(Err(serde_json::json!({ "kind": "rejected" })))
                }
            }
        })
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

#[derive(Debug)]
struct EmptyConsumerFactory;

impl ConformancePluginFactory for EmptyConsumerFactory {
    fn package_id(&self) -> &'static str {
        "test.bridge-consumer"
    }

    fn instantiate(&self, _: &PluginInstancePlan) -> Result<ConformancePlugin, RuntimeFailure> {
        Ok(ConformancePlugin::default())
    }
}

#[test]
fn guest_import_invokes_only_the_plan_bound_host_capability() {
    let source = br#"
        export function describe() {
          return JSON.stringify({
            abi: "lenso.json-host-imports@2",
            capabilities: [{
              capability_id: "test.bridge@1",
              descriptor_version: "1.0.0",
              request_operations: ["bridge"]
            }],
            required_capabilities: [{
              requirement_id: "~lenso.runtime.conformance.probe@1",
              capability_id: "lenso.runtime.conformance.probe@1",
              descriptor_version: "1.0.0",
              cardinality: "one"
            }]
          });
        }
        export function invoke(capability, operation, requestJson) {
          if (capability !== "test.bridge@1" || operation !== "bridge") throw new Error("identity");
          const bindings = JSON.parse(lensoHostBindings());
          if (bindings.runtime || bindings.ok.length !== 1) throw new Error("bindings");
          const binding = bindings.ok[0];
          if (binding.provider_instance !== "provider" ||
              binding.capability_id !== "lenso.runtime.conformance.probe@1") throw new Error("authority");
          const host = JSON.parse(lensoHostInvoke(
            binding.binding_id,
            "probe",
            JSON.stringify({value: JSON.parse(requestJson)})
          ));
          if (host.runtime) throw new Error(`runtime:${host.runtime.kind}`);
          if (host.error) return JSON.stringify({error: "probe-domain"});
          return JSON.stringify({ok: host.ok.value});
        }
        export function streamOpen() { return JSON.stringify({error: null}); }
        export function streamSend() { return JSON.stringify({ok: null}); }
        export function streamReceive() { return JSON.stringify({ok: {kind: "terminal-success"}}); }
        export function streamCloseSend() { return JSON.stringify({ok: null}); }
        export function streamCancel() { return JSON.stringify({ok: null}); }
    "#;
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), source).unwrap();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(source)));
    let artifact = ArtifactHandle::open(file.path(), &digest, source.len() as u64).unwrap();
    let quickjs = QuickJsAdapter::new(
        ArtifactCatalog::new()
            .with_artifact("plugin", artifact)
            .unwrap(),
    )
    .with_codec(BridgeCodec)
    .with_codec(ProbeCodec);
    let adapters = ExecutionAdapterCatalog::new()
        .with_adapter(
            ConformanceExecutionAdapter::new()
                .with_factory(ProbeProviderFactory)
                .with_factory(EmptyConsumerFactory),
        )
        .unwrap()
        .with_adapter(quickjs)
        .unwrap();
    let plan = guest_import_plan();
    let driver = DeterministicDriver::new();
    let app = driver
        .run(Kernel::start(plan, driver.clone(), adapters))
        .expect("the cross-runtime App should activate");
    let result = driver
        .run(
            app.handle::<Bridge>("consumer")
                .unwrap()
                .invoke("bridge", "hello".to_owned()),
        )
        .expect("the guest invocation should reach the host")
        .expect("the host should not return a Domain Error");
    assert_eq!(result, "Echo: hello");
}

#[test]
fn guest_import_preserves_host_stream_messages_and_terminal_protocol() {
    let source = br#"
        export function describe() {
          return JSON.stringify({
            abi: "lenso.json-host-imports@2",
            capabilities: [{
              capability_id: "test.bridge@1",
              descriptor_version: "1.0.0",
              request_operations: ["bridge"]
            }],
            required_capabilities: [{
              requirement_id: "~lenso.runtime.conformance.stream-probe@1",
              capability_id: "lenso.runtime.conformance.stream-probe@1",
              descriptor_version: "1.0.0",
              cardinality: "one"
            }]
          });
        }
        export function invoke(capability, operation, requestJson) {
          if (capability !== "test.bridge@1" || operation !== "bridge") throw new Error("identity");
          const binding = JSON.parse(lensoHostBindings()).ok[0];
          const opened = JSON.parse(lensoHostStreamOpen(
            binding.binding_id,
            "exchange",
            JSON.stringify({value: "room"})
          ));
          if (opened.runtime || opened.error) throw new Error("open");
          const id = opened.ok;
          const sent = JSON.parse(lensoHostStreamSend(
            id,
            JSON.stringify({sequence: 1, value: JSON.parse(requestJson)})
          ));
          if (sent.runtime) throw new Error("send");
          const message = JSON.parse(lensoHostStreamReceive(id));
          if (message.ok.kind !== "message" || message.ok.value.sequence !== 1) throw new Error("message");
          if (JSON.parse(lensoHostStreamCloseSend(id)).runtime) throw new Error("close");
          const halfClosed = JSON.parse(lensoHostStreamReceive(id));
          const terminal = JSON.parse(lensoHostStreamReceive(id));
          if (halfClosed.ok.kind !== "peer-half-closed" ||
              terminal.ok.kind !== "terminal-success") throw new Error("terminal");
          return JSON.stringify({ok: message.ok.value.value});
        }
        export function streamOpen() { return JSON.stringify({error: null}); }
        export function streamSend() { return JSON.stringify({ok: null}); }
        export function streamReceive() { return JSON.stringify({ok: {kind: "terminal-success"}}); }
        export function streamCloseSend() { return JSON.stringify({ok: null}); }
        export function streamCancel() { return JSON.stringify({ok: null}); }
    "#;
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), source).unwrap();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(source)));
    let artifact = ArtifactHandle::open(file.path(), &digest, source.len() as u64).unwrap();
    let quickjs = QuickJsAdapter::new(
        ArtifactCatalog::new()
            .with_artifact("plugin", artifact)
            .unwrap(),
    )
    .with_codec(BridgeCodec)
    .with_codec(StreamProbeCodec);
    let adapters = ExecutionAdapterCatalog::new()
        .with_adapter(
            ConformanceExecutionAdapter::new()
                .with_factory(StreamProbeProviderFactory)
                .with_factory(EmptyConsumerFactory),
        )
        .unwrap()
        .with_adapter(quickjs)
        .unwrap();
    let driver = DeterministicDriver::new();
    let app = driver
        .run(Kernel::start(
            guest_stream_import_plan(),
            driver.clone(),
            adapters,
        ))
        .expect("the host Stream import App should activate");
    let result = driver
        .run(
            app.handle::<Bridge>("consumer")
                .unwrap()
                .invoke("bridge", "hello".to_owned()),
        )
        .expect("the guest Stream import should succeed")
        .expect("the host Stream should not return a Domain Error");
    assert_eq!(result, "room: hello");
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
    assert!(matches!(failure, Err(RuntimeFailure::PluginFailure { .. })));
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
            PluginInstancePlan::new("plugin", "test.quickjs")
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
            PluginInstancePlan::new("plugin", "test.quickjs")
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

fn guest_import_plan() -> ResolvedAppPlan {
    AppComposition::new(
        vec![
            PluginInstancePlan::new("provider", PROBE_PROVIDER_PACKAGE_ID).with_capability(
                CapabilityEndpointPlan::new(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                    [PROBE_OPERATION],
                ),
            ),
            PluginInstancePlan::new("plugin", "test.quickjs")
                .with_entrypoint("plugin.mjs")
                .with_execution_class(ExecutionClassId::new(EXECUTION_CLASS))
                .with_requirement(CapabilityRequirementPlan::one(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                ))
                .with_capability(CapabilityEndpointPlan::new(
                    Bridge::ID,
                    Bridge::DESCRIPTOR_VERSION,
                    ["bridge"],
                )),
            PluginInstancePlan::new("consumer", "test.bridge-consumer").with_requirement(
                CapabilityRequirementPlan::one(Bridge::ID, Bridge::DESCRIPTOR_VERSION),
            ),
        ],
        vec![
            CapabilityBinding::new(
                "plugin",
                PROBE_CAPABILITY_ID,
                PROBE_DESCRIPTOR_VERSION,
                "provider",
            ),
            CapabilityBinding::new("consumer", Bridge::ID, Bridge::DESCRIPTOR_VERSION, "plugin"),
        ],
    )
    .resolve()
    .expect("the Guest Import fixture should resolve")
}

fn guest_stream_import_plan() -> ResolvedAppPlan {
    AppComposition::new(
        vec![
            PluginInstancePlan::new("provider", STREAM_PROBE_PROVIDER_PACKAGE_ID).with_capability(
                CapabilityEndpointPlan::new(
                    STREAM_PROBE_CAPABILITY_ID,
                    STREAM_PROBE_DESCRIPTOR_VERSION,
                    [STREAM_PROBE_OPERATION],
                )
                .with_stream_operation(STREAM_PROBE_OPERATION),
            ),
            PluginInstancePlan::new("plugin", "test.quickjs")
                .with_entrypoint("plugin.mjs")
                .with_execution_class(ExecutionClassId::new(EXECUTION_CLASS))
                .with_requirement(CapabilityRequirementPlan::one(
                    STREAM_PROBE_CAPABILITY_ID,
                    STREAM_PROBE_DESCRIPTOR_VERSION,
                ))
                .with_capability(CapabilityEndpointPlan::new(
                    Bridge::ID,
                    Bridge::DESCRIPTOR_VERSION,
                    ["bridge"],
                )),
            PluginInstancePlan::new("consumer", "test.bridge-consumer").with_requirement(
                CapabilityRequirementPlan::one(Bridge::ID, Bridge::DESCRIPTOR_VERSION),
            ),
        ],
        vec![
            CapabilityBinding::new(
                "plugin",
                STREAM_PROBE_CAPABILITY_ID,
                STREAM_PROBE_DESCRIPTOR_VERSION,
                "provider",
            ),
            CapabilityBinding::new("consumer", Bridge::ID, Bridge::DESCRIPTOR_VERSION, "plugin"),
        ],
    )
    .resolve()
    .expect("the Guest Stream Import fixture should resolve")
}
