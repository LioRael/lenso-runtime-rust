use std::{
    any::Any,
    fs,
    io::{Read as _, Write as _},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use lenso_app_plan::{
    CapabilityEndpointPlan, ExecutionClassId, PluginInstancePlan, ResolvedAppPlan,
};
use lenso_kernel::{CancellationToken, ExecutionAdapter, InvocationContext, RuntimeFailure};
use lenso_remote_adapter::{EXECUTION_CLASS, PROTOCOL_VERSION, RemoteAdapter, RemoteLimits};
use lenso_runtime_codec::{ArtifactCatalog, ArtifactHandle, JsonCapabilityCodec};
use reqwest::{blocking::Client, redirect};
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
fn non_ascii_provider_failure_is_truncated_on_a_utf8_boundary() {
    let server = RemoteServer::new(expected_descriptor());
    let generation = remote_generation(&server, RemoteLimits::default()).unwrap();

    let error = futures::executor::block_on(generation.endpoints()[0].invoke(
        "echo",
        Box::new(json!({"unicode_failure": true})),
        InvocationContext::new(29, None, CancellationToken::new()),
    ))
    .unwrap_err();

    let RuntimeFailure::PluginFailure { detail } = error else {
        panic!("provider failure should remain a Plugin Failure");
    };
    assert_eq!(detail.len(), 510);
    assert_eq!(detail.chars().count(), 170);
    assert!(detail.chars().all(|character| character == '界'));
}

#[test]
fn independent_requests_overlap_on_the_bounded_worker_pool() {
    let server = RemoteServer::new(expected_descriptor());
    let generation = remote_generation(&server, RemoteLimits::default()).unwrap();
    let endpoint = generation.endpoints()[0].clone();
    let first = endpoint.invoke(
        "echo",
        Box::new(json!({"delay_ms": 200})),
        InvocationContext::new(30, None, CancellationToken::new()),
    );
    let second = endpoint.invoke(
        "echo",
        Box::new(json!({"delay_ms": 200})),
        InvocationContext::new(31, None, CancellationToken::new()),
    );

    let started = Instant::now();
    let (first, second) = futures::executor::block_on(futures::future::join(first, second));

    assert!(first.is_ok());
    assert!(second.is_ok());
    assert!(
        started.elapsed() < Duration::from_millis(350),
        "two 200 ms requests should overlap"
    );
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
    assert_eq!(server.cancel_count.load(Ordering::Acquire), 1);
    assert_eq!(server.invocations.lock().unwrap().len(), 1);
}

#[test]
fn cancellation_drops_queued_work_before_http_dispatch() {
    let server = RemoteServer::new(expected_descriptor());
    let generation = remote_generation(&server, RemoteLimits::default()).unwrap();
    let endpoint = generation.endpoints()[0].clone();
    let blocked = (0..4)
        .map(|index| {
            endpoint.invoke(
                "echo",
                Box::new(json!({"block": true, "marker": format!("blocked-{index}")})),
                InvocationContext::new(40 + index, None, CancellationToken::new()),
            )
        })
        .collect::<Vec<_>>();
    let cancellation = CancellationToken::new();
    let cancel_after_queue = cancellation.clone();
    let release_blocked = Arc::clone(&server.release_blocked);
    let queued = endpoint.invoke(
        "echo",
        Box::new(json!({"marker": "must-not-dispatch"})),
        InvocationContext::new(50, None, cancellation),
    );

    let result = futures::executor::block_on(async move {
        let cancel = async move {
            thread::sleep(Duration::from_millis(100));
            cancel_after_queue.cancel();
            thread::sleep(Duration::from_millis(50));
            release_blocked.store(true, Ordering::Release);
        };
        let blocked = futures::future::join_all(blocked);
        let (_, queued, ()) = futures::join!(blocked, queued, cancel);
        queued
    });

    assert!(matches!(result, Err(RuntimeFailure::Cancelled { .. })));
    assert!(
        server
            .invocations
            .lock()
            .unwrap()
            .iter()
            .all(|request| { request["request"]["marker"] != "must-not-dispatch" })
    );
}

#[test]
fn queue_wait_counts_against_the_remote_request_timeout() {
    let server = RemoteServer::new(expected_descriptor());
    let limits = RemoteLimits {
        max_pending_requests: 8,
        request_timeout: Duration::from_millis(100),
        ..RemoteLimits::default()
    };
    let generation = remote_generation(&server, limits).unwrap();
    let endpoint = generation.endpoints()[0].clone();
    let blocked = (0..4)
        .map(|index| {
            endpoint.invoke(
                "echo",
                Box::new(json!({"delay_ms": 250, "marker": format!("slow-{index}")})),
                InvocationContext::new(60 + index, None, CancellationToken::new()),
            )
        })
        .collect::<Vec<_>>();
    let queued = endpoint.invoke(
        "echo",
        Box::new(json!({"marker": "expired-in-queue"})),
        InvocationContext::new(70, None, CancellationToken::new()),
    );

    let (_, queued) = futures::executor::block_on(futures::future::join(
        futures::future::join_all(blocked),
        queued,
    ));

    assert!(matches!(queued, Err(RuntimeFailure::PluginFailure { .. })));
    assert!(
        server
            .invocations
            .lock()
            .unwrap()
            .iter()
            .all(|request| request["request"]["marker"] != "expired-in-queue")
    );
}

#[test]
fn slow_cancel_backlog_is_bounded_and_fails_the_generation_closed() {
    let server = RemoteServer::with_cancel_delay(expected_descriptor(), Duration::from_secs(2));
    let limits = RemoteLimits {
        max_pending_requests: 1,
        request_timeout: Duration::from_secs(3),
        ..RemoteLimits::default()
    };
    let generation = remote_generation(&server, limits).unwrap();
    let endpoint = generation.endpoints()[0].clone();

    for index in 0_usize..4 {
        let cancellation = CancellationToken::new();
        let cancel_after_dispatch = cancellation.clone();
        let invocations = Arc::clone(&server.invocations);
        let invocation = endpoint.invoke(
            "echo",
            Box::new(json!({"block": true, "marker": format!("cancel-{index}")})),
            InvocationContext::new(80 + u64::try_from(index).unwrap(), None, cancellation),
        );
        let (result, ()) =
            futures::executor::block_on(futures::future::join(invocation, async move {
                while invocations.lock().unwrap().len() <= index {
                    thread::sleep(Duration::from_millis(2));
                }
                cancel_after_dispatch.cancel();
            }));
        assert!(matches!(result, Err(RuntimeFailure::Cancelled { .. })));
    }

    let rejected = futures::executor::block_on(endpoint.invoke(
        "echo",
        Box::new(json!({"marker": "after-cancel-overflow"})),
        InvocationContext::new(90, None, CancellationToken::new()),
    ));
    assert!(matches!(
        rejected,
        Err(RuntimeFailure::PluginFailure { .. })
    ));
    server.release_blocked.store(true, Ordering::Release);
    let shutdown_started = Instant::now();
    drop(endpoint);
    drop(generation);
    assert!(
        shutdown_started.elapsed() < Duration::from_millis(2_500),
        "two shutdown cancel workers must cap four retries at two one-second rounds"
    );
    assert_eq!(server.cancel_count.load(Ordering::Acquire), 6);
}

#[test]
fn default_remote_client_rejects_readiness_redirects() {
    let target = RemoteServer::new(expected_descriptor());
    let redirect = RedirectServer::new(format!("{}lenso/v1/ready", target.base_url));

    let result = remote_generation_at(&redirect.base_url, RemoteLimits::default());

    assert!(matches!(result, Err(RuntimeFailure::PluginFailure { .. })));
    assert!(target.invocations.lock().unwrap().is_empty());
}

#[test]
fn product_owned_http_client_explicitly_controls_redirect_policy() {
    let target = RemoteServer::new(expected_descriptor());
    let redirect = RedirectServer::new(format!("{}lenso/v1/ready", target.base_url));
    let client = Client::builder()
        .redirect(redirect::Policy::limited(1))
        .build()
        .unwrap();

    let generation =
        remote_generation_at_with_client(&redirect.base_url, RemoteLimits::default(), Some(client))
            .expect("the product-owned redirect policy should be authoritative");

    drop(generation);
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
    remote_generation_at(&server.base_url, limits)
}

fn remote_generation_at(
    base_url: &str,
    limits: RemoteLimits,
) -> Result<lenso_kernel::PreparedNativePlugin, RuntimeFailure> {
    remote_generation_at_with_client(base_url, limits, None)
}

fn remote_generation_at_with_client(
    base_url: &str,
    limits: RemoteLimits,
    client: Option<Client>,
) -> Result<lenso_kernel::PreparedNativePlugin, RuntimeFailure> {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("remote.json");
    let bytes = serde_json::to_vec(&json!({
        "schema_version": 1,
        "protocol": PROTOCOL_VERSION,
        "base_url": base_url,
    }))
    .unwrap();
    fs::write(&path, &bytes).unwrap();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
    let artifact = ArtifactHandle::open(&path, &digest, bytes.len() as u64).unwrap();
    let artifacts = ArtifactCatalog::new().with_artifact("plugin", artifact)?;
    let adapter = RemoteAdapter::new(artifacts)
        .with_codec(EchoCodec)
        .with_limits(limits);
    let adapter = if let Some(client) = client {
        adapter.with_http_client(client)
    } else {
        adapter
    };
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
    cancel_count: Arc<AtomicUsize>,
    release_blocked: Arc<AtomicBool>,
    invocations: Arc<Mutex<Vec<Value>>>,
    stopped: Arc<AtomicBool>,
    address: std::net::SocketAddr,
    acceptor: Option<thread::JoinHandle<()>>,
}

impl RemoteServer {
    fn new(descriptor: Value) -> Self {
        Self::with_cancel_delay(descriptor, Duration::ZERO)
    }

    fn with_cancel_delay(descriptor: Value, cancel_delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_count = Arc::new(AtomicUsize::new(0));
        let release_blocked = Arc::new(AtomicBool::new(false));
        let invocations = Arc::new(Mutex::new(Vec::new()));
        let stopped = Arc::new(AtomicBool::new(false));
        let thread_cancelled = cancelled.clone();
        let thread_cancel_count = Arc::clone(&cancel_count);
        let thread_release_blocked = Arc::clone(&release_blocked);
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
                let cancel_count = Arc::clone(&thread_cancel_count);
                let release_blocked = Arc::clone(&thread_release_blocked);
                let invocations = thread_invocations.clone();
                handlers.push(thread::spawn(move || {
                    handle_connection(
                        stream,
                        &descriptor,
                        &cancelled,
                        &cancel_count,
                        &release_blocked,
                        cancel_delay,
                        &invocations,
                    );
                }));
            }
            for handler in handlers {
                let _ = handler.join();
            }
        });
        Self {
            base_url: format!("http://{address}/"),
            cancelled,
            cancel_count,
            release_blocked,
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
        self.release_blocked.store(true, Ordering::Release);
        self.stopped.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
    }
}

struct RedirectServer {
    base_url: String,
    worker: Option<thread::JoinHandle<()>>,
}

impl RedirectServer {
    fn new(location: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_request(&mut stream);
            write!(
                stream,
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            stream.flush().unwrap();
        });
        Self {
            base_url: format!("http://{address}/"),
            worker: Some(worker),
        }
    }
}

impl Drop for RedirectServer {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    descriptor: &Value,
    cancelled: &AtomicBool,
    cancel_count: &AtomicUsize,
    release_blocked: &AtomicBool,
    cancel_delay: Duration,
    invocations: &Mutex<Vec<Value>>,
) {
    let (path, body) = read_request(&mut stream);
    let response = match path.as_str() {
        "/lenso/v1/ready" => json!({
            "protocol": PROTOCOL_VERSION,
            "descriptor": descriptor,
        }),
        "/lenso/v1/cancel" => {
            cancel_count.fetch_add(1, Ordering::AcqRel);
            thread::sleep(cancel_delay);
            cancelled.store(true, Ordering::Release);
            json!({"cancelled": true})
        }
        "/lenso/v1/invoke" => {
            let request: Value = serde_json::from_slice(&body).unwrap();
            invocations.lock().unwrap().push(request.clone());
            let generation_id = &request["generation_id"];
            let request_id = &request["request_id"];
            if request["request"]["block"] == true {
                while !cancelled.load(Ordering::Acquire) && !release_blocked.load(Ordering::Acquire)
                {
                    thread::sleep(Duration::from_millis(5));
                }
            }
            if let Some(delay_ms) = request["request"]["delay_ms"].as_u64() {
                thread::sleep(Duration::from_millis(delay_ms));
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
            } else if request["request"]["unicode_failure"] == true {
                json!({
                    "protocol": PROTOCOL_VERSION,
                    "generation_id": generation_id,
                    "request_id": request_id,
                    "failure": "界".repeat(200)
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
