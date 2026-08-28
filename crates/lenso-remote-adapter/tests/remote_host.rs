use std::{
    any::Any,
    fs,
    io::{Read as _, Write as _},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use lenso_app_plan::{
    CapabilityEndpointPlan, ExecutionClassId, PluginInstancePlan, ResolvedAppPlan,
};
use lenso_kernel::{CancellationToken, ExecutionAdapter, InvocationContext, RuntimeFailure};
use lenso_remote_adapter::{EXECUTION_CLASS, PROTOCOL_VERSION, RemoteAdapter, RemoteLimits};
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
fn readiness_success_and_domain_error_cross_a_real_http_boundary() {
    let server = RemoteServer::new(expected_descriptor());
    let generation = remote_generation(&server, RemoteLimits::default()).unwrap();

    let success = futures::executor::block_on(generation.endpoints()[0].invoke(
        "echo",
        Box::new(json!({"message": "hello"})),
        InvocationContext::new(1, None, CancellationToken::new()),
    ))
    .unwrap()
    .unwrap()
    .downcast::<Value>()
    .unwrap();
    assert_eq!(*success, json!({"message": "hello"}));

    let error = futures::executor::block_on(generation.endpoints()[0].invoke(
        "echo",
        Box::new(json!({"domain_error": true})),
        InvocationContext::new(2, None, CancellationToken::new()),
    ))
    .unwrap()
    .unwrap_err()
    .downcast::<Value>()
    .unwrap();
    assert_eq!(*error, json!({"kind": "declared"}));
}

#[test]
fn cancellation_is_propagated_without_replaying_the_request() {
    let server = RemoteServer::new(expected_descriptor());
    let generation = remote_generation(&server, RemoteLimits::default()).unwrap();
    let cancellation = CancellationToken::new();
    let cancel_after_admission = cancellation.clone();
    let invocation = generation.endpoints()[0].invoke(
        "echo",
        Box::new(json!({"block": true})),
        InvocationContext::new(3, None, cancellation),
    );
    let (result, ()) = futures::executor::block_on(futures::future::join(invocation, async move {
        thread::sleep(Duration::from_millis(100));
        cancel_after_admission.cancel();
    }));

    assert!(matches!(result, Err(RuntimeFailure::Cancelled { .. })));
    for _ in 0..100 {
        if server.cancelled.load(Ordering::Acquire) {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(server.cancelled.load(Ordering::Acquire));
    assert_eq!(server.invocations.lock().unwrap().len(), 1);
}

#[test]
fn descriptor_mismatch_fails_before_an_endpoint_becomes_ready() {
    let server = RemoteServer::new(json!({
        "abi": "lenso.json-request@1",
        "capabilities": []
    }));
    let result = remote_generation(&server, RemoteLimits::default());

    assert!(matches!(
        result,
        Err(RuntimeFailure::InvalidResolvedPlan { .. })
    ));
}

#[test]
fn oversized_response_is_bounded_by_host_policy() {
    let server = RemoteServer::new(expected_descriptor());
    let limits = RemoteLimits {
        max_response_bytes: 256,
        ..RemoteLimits::default()
    };
    let generation = remote_generation(&server, limits).unwrap();
    let result = futures::executor::block_on(generation.endpoints()[0].invoke(
        "echo",
        Box::new(json!({"large": true})),
        InvocationContext::new(4, None, CancellationToken::new()),
    ));

    assert!(matches!(
        result,
        Err(RuntimeFailure::ResourceExhausted { .. })
    ));
}

#[test]
fn oversized_request_is_rejected_before_network_dispatch() {
    let server = RemoteServer::new(expected_descriptor());
    let limits = RemoteLimits {
        max_request_bytes: 128,
        ..RemoteLimits::default()
    };
    let generation = remote_generation(&server, limits).unwrap();
    let result = futures::executor::block_on(generation.endpoints()[0].invoke(
        "echo",
        Box::new(json!({"message": "x".repeat(512)})),
        InvocationContext::new(5, None, CancellationToken::new()),
    ));

    assert!(matches!(
        result,
        Err(RuntimeFailure::ResourceExhausted { .. })
    ));
    assert!(server.invocations.lock().unwrap().is_empty());
}

fn remote_generation(
    server: &RemoteServer,
    limits: RemoteLimits,
) -> Result<lenso_kernel::PreparedNativePlugin, RuntimeFailure> {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("remote.json");
    let bytes = serde_json::to_vec(&json!({
        "schema_version": 1,
        "protocol": PROTOCOL_VERSION,
        "base_url": server.base_url,
    }))
    .unwrap();
    fs::write(&path, &bytes).unwrap();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
    let artifact = ArtifactHandle::open(&path, &digest, bytes.len() as u64).unwrap();
    let artifacts = ArtifactCatalog::new().with_artifact("plugin", artifact)?;
    let adapter = RemoteAdapter::new(artifacts)
        .with_codec(EchoCodec)
        .with_limits(limits);
    adapter.recreate(&remote_plan(), "plugin")
}

fn remote_plan() -> ResolvedAppPlan {
    ResolvedAppPlan::new(
        vec![
            PluginInstancePlan::new("plugin", "example.remote")
                .with_entrypoint("plugin")
                .with_execution_class(ExecutionClassId::new(EXECUTION_CLASS))
                .with_capability(CapabilityEndpointPlan::new(
                    "example.echo@1",
                    "1.0.0",
                    ["echo"],
                )),
        ],
        Vec::new(),
    )
}

fn expected_descriptor() -> Value {
    json!({
        "abi": "lenso.json-request@1",
        "capabilities": [{
            "capability_id": "example.echo@1",
            "descriptor_version": "1.0.0",
            "request_operations": ["echo"],
            "stream_operations": []
        }]
    })
}

struct RemoteServer {
    base_url: String,
    cancelled: Arc<AtomicBool>,
    invocations: Arc<Mutex<Vec<Value>>>,
    stopped: Arc<AtomicBool>,
    address: std::net::SocketAddr,
    acceptor: Option<thread::JoinHandle<()>>,
}

impl RemoteServer {
    fn new(descriptor: Value) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let invocations = Arc::new(Mutex::new(Vec::new()));
        let stopped = Arc::new(AtomicBool::new(false));
        let thread_cancelled = cancelled.clone();
        let thread_invocations = invocations.clone();
        let thread_stopped = stopped.clone();
        let acceptor = thread::spawn(move || {
            let mut handlers = Vec::new();
            while !thread_stopped.load(Ordering::Acquire) {
                let Ok((stream, _)) = listener.accept() else {
                    break;
                };
                if thread_stopped.load(Ordering::Acquire) {
                    break;
                }
                let descriptor = descriptor.clone();
                let cancelled = thread_cancelled.clone();
                let invocations = thread_invocations.clone();
                handlers.push(thread::spawn(move || {
                    handle_connection(stream, &descriptor, &cancelled, &invocations);
                }));
            }
            for handler in handlers {
                let _ = handler.join();
            }
        });
        Self {
            base_url: format!("http://{address}/"),
            cancelled,
            invocations,
            stopped,
            address,
            acceptor: Some(acceptor),
        }
    }
}

impl Drop for RemoteServer {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        self.stopped.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    descriptor: &Value,
    cancelled: &AtomicBool,
    invocations: &Mutex<Vec<Value>>,
) {
    let (path, body) = read_request(&mut stream);
    let response = match path.as_str() {
        "/lenso/v1/ready" => json!({
            "protocol": PROTOCOL_VERSION,
            "descriptor": descriptor,
        }),
        "/lenso/v1/cancel" => {
            cancelled.store(true, Ordering::Release);
            json!({"cancelled": true})
        }
        "/lenso/v1/invoke" => {
            let request: Value = serde_json::from_slice(&body).unwrap();
            invocations.lock().unwrap().push(request.clone());
            let generation_id = &request["generation_id"];
            let request_id = &request["request_id"];
            if request["request"]["block"] == true {
                while !cancelled.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(5));
                }
            }
            if request["request"]["large"] == true {
                json!({
                    "protocol": PROTOCOL_VERSION,
                    "generation_id": generation_id,
                    "request_id": request_id,
                    "ok": "x".repeat(1024)
                })
            } else if request["request"]["domain_error"] == true {
                json!({
                    "protocol": PROTOCOL_VERSION,
                    "generation_id": generation_id,
                    "request_id": request_id,
                    "error": {"kind": "declared"}
                })
            } else {
                json!({
                    "protocol": PROTOCOL_VERSION,
                    "generation_id": generation_id,
                    "request_id": request_id,
                    "ok": request["request"]
                })
            }
        }
        _ => json!({"failure": "not found"}),
    };
    write_response(&mut stream, &response);
}

fn read_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    let header_end = loop {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    let path = headers.split_whitespace().nth(1).unwrap().to_owned();
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(str::trim)
                .map(str::parse::<usize>)
        })
        .transpose()
        .unwrap()
        .unwrap_or(0);
    while bytes.len() - header_end < content_length {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&buffer[..read]);
    }
    (
        path,
        bytes[header_end..header_end + content_length].to_vec(),
    )
}

fn write_response(stream: &mut TcpStream, value: &Value) {
    let body = serde_json::to_vec(value).unwrap();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
    stream.flush().unwrap();
}
