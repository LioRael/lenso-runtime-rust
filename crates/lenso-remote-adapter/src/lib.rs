//! Generation-bound HTTP Execution Adapter for remote Plugin deployments.
//!
//! A deployment binding is admitted as the Instance Artifact, so its endpoint
//! participates in the same immutable Generation authority as executable
//! Artifacts. Remote V1 supports request providers only, performs an exact
//! readiness handshake, propagates cancellation, and never retries invocation.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read as _,
    net::IpAddr,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use futures::{FutureExt as _, select};
use lenso_app_plan::{ExecutionClassId, PluginInstancePlan, ResolvedAppPlan};
use lenso_kernel::{
    ExecutionAdapter, InvocationContext, PluginLifecycle, PreparedNativeApp, PreparedNativePlugin,
    RuntimeFailure,
};
use lenso_runtime_codec::{
    ArtifactCatalog, JsonCapabilityCodec, JsonInvocationOutcome, JsonRequestTransport,
    codecs_for_instance, json_request_endpoints, prepare_request_app,
    validate_json_plugin_descriptor,
};
use reqwest::{StatusCode, Url, blocking::Client, redirect};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, value::RawValue};

/// Stable remote HTTP execution class.
pub const EXECUTION_CLASS: &str = "lenso.remote@1";
/// Exact runtime profile implemented by this Adapter release.
pub const RUNTIME_PROFILE: &str = "lenso.remote@1";
/// Exact wire protocol spoken by Host and remote deployment.
pub const PROTOCOL_VERSION: &str = "lenso.remote-http-json@1";

/// Host-owned resource and time limits for one remote generation.
#[derive(Clone, Debug)]
pub struct RemoteLimits {
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    pub max_pending_requests: usize,
    pub startup_timeout: Duration,
    pub request_timeout: Duration,
}

impl Default for RemoteLimits {
    fn default() -> Self {
        Self {
            max_request_bytes: 1024 * 1024,
            max_response_bytes: 1024 * 1024,
            max_pending_requests: 64,
            startup_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(30),
        }
    }
}

/// Adapter configured with admitted deployment bindings and generated codecs.
#[derive(Debug)]
pub struct RemoteAdapter {
    artifacts: ArtifactCatalog,
    client: Option<Client>,
    codecs: BTreeMap<String, Rc<dyn JsonCapabilityCodec>>,
    duplicate_codecs: BTreeSet<String>,
    limits: RemoteLimits,
}

impl RemoteAdapter {
    pub fn new(artifacts: ArtifactCatalog) -> Self {
        Self {
            artifacts,
            client: None,
            codecs: BTreeMap::new(),
            duplicate_codecs: BTreeSet::new(),
            limits: RemoteLimits::default(),
        }
    }

    #[must_use]
    pub fn with_codec(mut self, codec: impl JsonCapabilityCodec) -> Self {
        let capability = codec.capability_id().to_owned();
        if self
            .codecs
            .insert(capability.clone(), Rc::new(codec))
            .is_some()
        {
            self.duplicate_codecs.insert(capability);
        }
        self
    }

    #[must_use]
    pub fn with_shared_codec(mut self, codec: Rc<dyn JsonCapabilityCodec>) -> Self {
        let capability = codec.capability_id().to_owned();
        if self.codecs.insert(capability.clone(), codec).is_some() {
            self.duplicate_codecs.insert(capability);
        }
        self
    }

    #[must_use]
    pub fn with_limits(mut self, limits: RemoteLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Installs a product-owned client for proxy, identity, mTLS, and redirect policy.
    /// The built-in client rejects redirects; supplying a client explicitly transfers redirect
    /// authority to the product Host. Per-request Adapter timeouts remain authoritative.
    #[must_use]
    pub fn with_http_client(mut self, client: Client) -> Self {
        self.client = Some(client);
        self
    }

    fn prepare_instance(
        &self,
        instance: &PluginInstancePlan,
    ) -> Result<PreparedNativePlugin, RuntimeFailure> {
        if instance.runtime_profile() != RUNTIME_PROFILE {
            return invalid(format!(
                "Remote Adapter does not support runtime profile `{}`",
                instance.runtime_profile()
            ));
        }
        if instance.entrypoint() != "plugin" {
            return invalid(format!(
                "Remote Plugin Instance `{}` needs entrypoint `plugin`",
                instance.instance_key()
            ));
        }
        if !instance.required_capabilities().is_empty()
            || instance
                .provided_capabilities()
                .iter()
                .any(|capability| !capability.stream_operations().is_empty())
        {
            return invalid("Remote V1 supports request-only providers without Host imports");
        }
        if !self.duplicate_codecs.is_empty() {
            return invalid(format!(
                "duplicate generated codecs registered for {:?}",
                self.duplicate_codecs
            ));
        }
        let binding_bytes = self
            .artifacts
            .require(instance.instance_key())?
            .read_verified()?;
        let binding: DeploymentBinding = decode_strict(&binding_bytes).map_err(|detail| {
            RuntimeFailure::InvalidResolvedPlan {
                detail: format!("invalid Remote deployment binding: {detail}"),
            }
        })?;
        binding.validate()?;
        let codecs = codecs_for_instance(instance, &self.codecs)?;
        let generation =
            RemoteGeneration::start(&binding, instance, self.client.clone(), self.limits.clone())?;
        let endpoints = json_request_endpoints(generation.clone(), codecs);
        Ok(PreparedNativePlugin::with_endpoints(
            endpoints,
            Vec::new(),
            RemoteLifecycle { generation },
        ))
    }
}

impl ExecutionAdapter for RemoteAdapter {
    fn execution_class(&self) -> ExecutionClassId {
        ExecutionClassId::new(EXECUTION_CLASS)
    }

    fn prepare(&self, plan: &ResolvedAppPlan) -> Result<PreparedNativeApp, RuntimeFailure> {
        plan.validate()
            .map_err(|error| RuntimeFailure::InvalidResolvedPlan {
                detail: error.to_string(),
            })?;
        let execution_class = self.execution_class();
        let mut generations = BTreeMap::new();
        for instance in plan
            .plugin_instances()
            .iter()
            .filter(|instance| instance.execution_class() == &execution_class)
        {
            let generation = self.prepare_instance(instance)?;
            if generations
                .insert(instance.instance_key().to_owned(), generation)
                .is_some()
            {
                return invalid(format!("duplicate Instance `{}`", instance.instance_key()));
            }
        }
        prepare_request_app(plan, &execution_class, generations)
    }

    fn recreate(
        &self,
        plan: &ResolvedAppPlan,
        instance_key: &str,
    ) -> Result<PreparedNativePlugin, RuntimeFailure> {
        let instance = plan.plugin_instance(instance_key).ok_or_else(|| {
            RuntimeFailure::InvalidResolvedPlan {
                detail: format!("unknown Instance `{instance_key}`"),
            }
        })?;
        if instance.execution_class().as_str() != EXECUTION_CLASS {
            return invalid(format!("Instance `{instance_key}` is not a Remote Plugin"));
        }
        self.prepare_instance(instance)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeploymentBinding {
    schema_version: u32,
    protocol: String,
    base_url: String,
}

impl DeploymentBinding {
    fn validate(&self) -> Result<(), RuntimeFailure> {
        if self.schema_version != 1 || self.protocol != PROTOCOL_VERSION {
            return invalid("unsupported Remote deployment binding version or protocol");
        }
        parse_base_url(&self.base_url).map(|_| ())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadyResponse {
    protocol: String,
    descriptor: Value,
}

#[derive(Debug, Serialize)]
struct InvokeRequest<'a> {
    protocol: &'static str,
    generation_id: &'a str,
    request_id: u64,
    capability: &'a str,
    operation: &'a str,
    request: &'a RawValue,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InvokeResponse {
    protocol: String,
    generation_id: String,
    request_id: u64,
    #[serde(default)]
    ok: Option<Value>,
    #[serde(default)]
    error: Option<Value>,
    #[serde(default)]
    failure: Option<String>,
}

#[derive(Debug, Serialize)]
struct CancelRequest<'a> {
    protocol: &'static str,
    generation_id: &'a str,
    request_id: u64,
}

struct RemoteGeneration {
    generation_id: String,
    pending: RemotePending,
    uncertain_cancels: RemoteUncertainCancels,
    workers: RemoteWorkerPool,
    next_id: AtomicU64,
    stopped: AtomicBool,
    limits: RemoteLimits,
}

impl std::fmt::Debug for RemoteGeneration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteGeneration")
            .field("generation_id", &self.generation_id)
            .field("stopped", &self.stopped)
            .finish_non_exhaustive()
    }
}

impl RemoteGeneration {
    fn start(
        binding: &DeploymentBinding,
        instance: &PluginInstancePlan,
        client: Option<Client>,
        limits: RemoteLimits,
    ) -> Result<Rc<Self>, RuntimeFailure> {
        let base_url = parse_base_url(&binding.base_url)?;
        let ready_url = base_url.join("lenso/v1/ready").map_err(invalid_url)?;
        let invoke_url = base_url.join("lenso/v1/invoke").map_err(invalid_url)?;
        let cancel_url = base_url.join("lenso/v1/cancel").map_err(invalid_url)?;
        let client = match client {
            Some(client) => client,
            None => Client::builder()
                .connect_timeout(limits.startup_timeout)
                .timeout(limits.request_timeout)
                .redirect(redirect::Policy::none())
                .build()
                .map_err(remote_internal)?,
        };
        let response = client
            .get(ready_url)
            .timeout(limits.startup_timeout)
            .send()
            .map_err(|error| remote_failure("readiness request failed", &error))?;
        let ready: ReadyResponse = read_json_response(response, limits.max_response_bytes)?;
        if ready.protocol != PROTOCOL_VERSION {
            return Err(RuntimeFailure::PluginFailure {
                detail: bounded(format!("unsupported Remote protocol `{}`", ready.protocol)),
            });
        }
        let descriptor = serde_json::to_string(&ready.descriptor).map_err(remote_internal)?;
        validate_json_plugin_descriptor(instance, &descriptor)?;
        let generation_id = uuid::Uuid::new_v4().to_string();
        let pending = RemotePending::default();
        let uncertain_cancels = RemoteUncertainCancels::default();
        let workers = RemoteWorkerPool::start(
            client.clone(),
            &invoke_url,
            cancel_url.clone(),
            generation_id.clone(),
            &pending,
            &uncertain_cancels,
            &limits,
        )?;
        Ok(Rc::new(Self {
            generation_id,
            pending,
            uncertain_cancels,
            workers,
            next_id: AtomicU64::new(1),
            stopped: AtomicBool::new(false),
            limits,
        }))
    }

    fn spawn_cancel(&self, request_id: u64) {
        if !self.workers.cancel(request_id) {
            self.stopped.store(true, Ordering::Release);
            let pending = self.pending.lock().expect("remote pending");
            let mut uncertain_cancels = self
                .uncertain_cancels
                .lock()
                .expect("remote uncertain cancels");
            for (pending_request_id, state) in pending.iter() {
                if state.swap(REMOTE_WORK_ABANDONED, Ordering::AcqRel) == REMOTE_WORK_DISPATCHED {
                    uncertain_cancels.insert(*pending_request_id);
                }
            }
        }
    }

    fn begin_stop(&self) -> Option<RemoteShutdownJob> {
        self.stopped.store(true, Ordering::Release);
        let pending = std::mem::take(&mut *self.pending.lock().expect("remote pending"));
        let mut shutdown_cancels = std::mem::take(
            &mut *self
                .uncertain_cancels
                .lock()
                .expect("remote uncertain cancels"),
        );
        for (request_id, state) in pending {
            if state.swap(REMOTE_WORK_ABANDONED, Ordering::AcqRel) == REMOTE_WORK_DISPATCHED {
                shutdown_cancels.insert(request_id);
            }
        }
        self.workers.begin_stop(
            shutdown_cancels
                .into_iter()
                .take(REMOTE_INVOKE_WORKERS)
                .collect(),
        )
    }

    fn stop(&self) {
        if let Some(job) = self.begin_stop() {
            job.run();
        }
    }
}

const REMOTE_INVOKE_WORKERS: usize = 4;
const REMOTE_CANCEL_WORKERS: usize = 2;
const REMOTE_WORKER_STOP_POLL_INTERVAL: Duration = Duration::from_millis(10);
const REMOTE_CANCEL_TIMEOUT: Duration = Duration::from_secs(1);
const REMOTE_WORK_QUEUED: u8 = 0;
const REMOTE_WORK_DISPATCHED: u8 = 1;
const REMOTE_WORK_ABANDONED: u8 = 2;
const REMOTE_WORK_FINISHED: u8 = 3;
type RemotePending = Arc<Mutex<BTreeMap<u64, Arc<AtomicU8>>>>;
type RemoteUncertainCancels = Arc<Mutex<BTreeSet<u64>>>;

struct RemoteInvokeWork {
    request_id: u64,
    context_request_id: u64,
    body: Vec<u8>,
    state: Arc<AtomicU8>,
    queued_at: Instant,
    outcome: futures::channel::oneshot::Sender<RemoteResult>,
}

struct RemoteWorkerPool {
    invokes: mpsc::SyncSender<RemoteInvokeWork>,
    cancels: mpsc::SyncSender<u64>,
    invoke_stopped: Arc<AtomicBool>,
    cancel_stopped: Arc<AtomicBool>,
    cancel_client: Client,
    cancel_url: Url,
    generation_id: String,
    cancel_timeout: Duration,
    workers: Mutex<Vec<thread::JoinHandle<()>>>,
    stopped: AtomicBool,
}

impl std::fmt::Debug for RemoteWorkerPool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteWorkerPool")
            .field("stopped", &self.stopped)
            .finish_non_exhaustive()
    }
}

impl RemoteWorkerPool {
    #[allow(clippy::too_many_lines)]
    fn start(
        client: Client,
        invoke_url: &Url,
        cancel_url: Url,
        generation_id: String,
        pending: &RemotePending,
        uncertain_cancels: &RemoteUncertainCancels,
        limits: &RemoteLimits,
    ) -> Result<Self, RuntimeFailure> {
        let capacity = limits.max_pending_requests.max(1);
        let (invokes, invoke_receiver) = mpsc::sync_channel::<RemoteInvokeWork>(capacity);
        let invoke_receiver = Arc::new(Mutex::new(invoke_receiver));
        let invoke_stopped = Arc::new(AtomicBool::new(false));
        let mut workers = Vec::with_capacity(REMOTE_INVOKE_WORKERS + REMOTE_CANCEL_WORKERS);
        for index in 0..REMOTE_INVOKE_WORKERS {
            let receiver = Arc::clone(&invoke_receiver);
            let client = client.clone();
            let url = invoke_url.clone();
            let expected_generation_id = generation_id.clone();
            let pending = Arc::clone(pending);
            let uncertain_cancels = Arc::clone(uncertain_cancels);
            let stopped = Arc::clone(&invoke_stopped);
            let timeout = limits.request_timeout;
            let max_response_bytes = limits.max_response_bytes;
            let worker = thread::Builder::new()
                .name(format!("lenso-remote-invoke-{index}"))
                .spawn(move || {
                    loop {
                        if stopped.load(Ordering::Acquire) {
                            return;
                        }
                        let work = {
                            let receiver = receiver.lock().expect("remote invoke queue");
                            if stopped.load(Ordering::Acquire) {
                                return;
                            }
                            match receiver.recv_timeout(REMOTE_WORKER_STOP_POLL_INTERVAL) {
                                Ok(work) => work,
                                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                                Err(mpsc::RecvTimeoutError::Disconnected) => return,
                            }
                        };
                        if stopped.load(Ordering::Acquire) {
                            return;
                        }
                        let elapsed = work.queued_at.elapsed();
                        let result = if elapsed >= timeout {
                            work.state.store(REMOTE_WORK_FINISHED, Ordering::Release);
                            Err(RuntimeFailure::PluginFailure {
                                detail: "Remote Plugin invocation timed out before dispatch"
                                    .to_owned(),
                            })
                        } else if work
                            .state
                            .compare_exchange(
                                REMOTE_WORK_QUEUED,
                                REMOTE_WORK_DISPATCHED,
                                Ordering::AcqRel,
                                Ordering::Acquire,
                            )
                            .is_err()
                        {
                            Err(RuntimeFailure::Cancelled {
                                request_id: work.context_request_id,
                            })
                        } else {
                            let result = client
                                .post(url.clone())
                                .timeout(
                                    timeout
                                        .checked_sub(elapsed)
                                        .expect("queue elapsed time was checked"),
                                )
                                .header(reqwest::header::CONTENT_TYPE, "application/json")
                                .body(work.body)
                                .send()
                                .map_err(|error| {
                                    remote_failure("invocation request failed", &error)
                                })
                                .and_then(|response| {
                                    read_json_response::<InvokeResponse>(
                                        response,
                                        max_response_bytes,
                                    )
                                })
                                .and_then(|response| {
                                    decode_invoke_response(
                                        response,
                                        &expected_generation_id,
                                        work.request_id,
                                    )
                                });
                            work.state.store(REMOTE_WORK_FINISHED, Ordering::Release);
                            result
                        };
                        pending
                            .lock()
                            .expect("remote pending")
                            .remove(&work.request_id);
                        uncertain_cancels
                            .lock()
                            .expect("remote uncertain cancels")
                            .remove(&work.request_id);
                        let _ = work.outcome.send(result);
                    }
                })
                .map_err(remote_internal)?;
            workers.push(worker);
        }

        let (cancels, cancel_receiver) = mpsc::sync_channel(capacity);
        let cancel_receiver = Arc::new(Mutex::new(cancel_receiver));
        let cancel_stopped = Arc::new(AtomicBool::new(false));
        let cancel_timeout = limits.request_timeout.min(REMOTE_CANCEL_TIMEOUT);
        for index in 0..REMOTE_CANCEL_WORKERS {
            let client = client.clone();
            let url = cancel_url.clone();
            let generation_id = generation_id.clone();
            let receiver = Arc::clone(&cancel_receiver);
            let uncertain_cancels = Arc::clone(uncertain_cancels);
            let stopped = Arc::clone(&cancel_stopped);
            let worker = thread::Builder::new()
                .name(format!("lenso-remote-cancel-{index}"))
                .spawn(move || {
                    loop {
                        if stopped.load(Ordering::Acquire) {
                            return;
                        }
                        let command = {
                            let receiver = receiver.lock().expect("remote cancel queue");
                            if stopped.load(Ordering::Acquire) {
                                return;
                            }
                            receiver.recv_timeout(REMOTE_WORKER_STOP_POLL_INTERVAL)
                        };
                        let request_id = match command {
                            Ok(request_id) => request_id,
                            Err(mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(mpsc::RecvTimeoutError::Disconnected) => return,
                        };
                        if stopped.load(Ordering::Acquire) {
                            return;
                        }
                        if send_remote_cancel(
                            &client,
                            &url,
                            &generation_id,
                            request_id,
                            cancel_timeout,
                        ) {
                            uncertain_cancels
                                .lock()
                                .expect("remote uncertain cancels")
                                .remove(&request_id);
                        }
                    }
                })
                .map_err(remote_internal)?;
            workers.push(worker);
        }

        Ok(Self {
            invokes,
            cancels,
            invoke_stopped,
            cancel_stopped,
            cancel_client: client,
            cancel_url,
            generation_id,
            cancel_timeout,
            workers: Mutex::new(workers),
            stopped: AtomicBool::new(false),
        })
    }

    fn invoke(&self, work: RemoteInvokeWork) -> bool {
        self.invokes.try_send(work).is_ok()
    }

    fn cancel(&self, request_id: u64) -> bool {
        self.cancels.try_send(request_id).is_ok()
    }

    fn begin_stop(&self, shutdown_cancels: Vec<u64>) -> Option<RemoteShutdownJob> {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return None;
        }
        Some(RemoteShutdownJob {
            invoke_stopped: Arc::clone(&self.invoke_stopped),
            cancel_stopped: Arc::clone(&self.cancel_stopped),
            cancel_client: self.cancel_client.clone(),
            cancel_url: self.cancel_url.clone(),
            generation_id: self.generation_id.clone(),
            cancel_timeout: self.cancel_timeout,
            shutdown_cancels,
            workers: std::mem::take(&mut *self.workers.lock().expect("remote workers")),
        })
    }
}

fn send_remote_cancel(
    client: &Client,
    url: &Url,
    generation_id: &str,
    request_id: u64,
    timeout: Duration,
) -> bool {
    client
        .post(url.clone())
        .timeout(timeout)
        .json(&CancelRequest {
            protocol: PROTOCOL_VERSION,
            generation_id,
            request_id,
        })
        .send()
        .is_ok_and(|response| response.status().is_success())
}

struct RemoteShutdownJob {
    invoke_stopped: Arc<AtomicBool>,
    cancel_stopped: Arc<AtomicBool>,
    cancel_client: Client,
    cancel_url: Url,
    generation_id: String,
    cancel_timeout: Duration,
    shutdown_cancels: Vec<u64>,
    workers: Vec<thread::JoinHandle<()>>,
}

impl RemoteShutdownJob {
    fn run(self) {
        self.invoke_stopped.store(true, Ordering::Release);
        self.cancel_stopped.store(true, Ordering::Release);
        send_shutdown_cancels(
            &self.cancel_client,
            &self.cancel_url,
            &self.generation_id,
            self.cancel_timeout,
            &self.shutdown_cancels,
        );
        for worker in self.workers {
            let _ = worker.join();
        }
    }
}

fn send_shutdown_cancels(
    client: &Client,
    url: &Url,
    generation_id: &str,
    timeout: Duration,
    request_ids: &[u64],
) {
    if request_ids.is_empty() {
        return;
    }
    let batch_size = request_ids.len().div_ceil(REMOTE_CANCEL_WORKERS);
    thread::scope(|scope| {
        let mut workers = Vec::with_capacity(REMOTE_CANCEL_WORKERS);
        for (index, batch) in request_ids.chunks(batch_size).enumerate() {
            let client = client.clone();
            let url = url.clone();
            if let Ok(worker) = thread::Builder::new()
                .name(format!("lenso-remote-shutdown-cancel-{index}"))
                .spawn_scoped(scope, move || {
                    for request_id in batch {
                        let _ =
                            send_remote_cancel(&client, &url, generation_id, *request_id, timeout);
                    }
                })
            {
                workers.push(worker);
            }
        }
        for worker in workers {
            let _ = worker.join();
        }
    });
}

struct RemotePendingGuard {
    generation: Rc<RemoteGeneration>,
    request_id: u64,
    state: Arc<AtomicU8>,
    armed: bool,
}

impl RemotePendingGuard {
    fn new(generation: Rc<RemoteGeneration>, request_id: u64, state: Arc<AtomicU8>) -> Self {
        Self {
            generation,
            request_id,
            state,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RemotePendingGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let previous = self.state.swap(REMOTE_WORK_ABANDONED, Ordering::AcqRel);
        let needs_cancel = {
            let mut pending = self.generation.pending.lock().expect("remote pending");
            let removed = pending.remove(&self.request_id).is_some();
            let needs_cancel = removed && previous == REMOTE_WORK_DISPATCHED;
            if needs_cancel {
                self.generation
                    .uncertain_cancels
                    .lock()
                    .expect("remote uncertain cancels")
                    .insert(self.request_id);
            }
            needs_cancel
        };
        if needs_cancel {
            self.generation.spawn_cancel(self.request_id);
        }
    }
}

type RemoteResult = Result<JsonInvocationOutcome, RuntimeFailure>;

impl JsonRequestTransport for RemoteGeneration {
    fn invoke(
        self: Rc<Self>,
        capability: String,
        operation: String,
        request_json: String,
        context: InvocationContext,
    ) -> futures::future::LocalBoxFuture<'static, RemoteResult> {
        Box::pin(async move {
            if self.stopped.load(Ordering::Acquire) {
                return Err(RuntimeFailure::PluginFailure {
                    detail: "Remote Plugin generation is unavailable".to_owned(),
                });
            }
            if request_json.len() > self.limits.max_request_bytes {
                return Err(RuntimeFailure::ResourceExhausted {
                    capability: EXECUTION_CLASS,
                    operation,
                });
            }
            let request = RawValue::from_string(request_json).map_err(|_| {
                RuntimeFailure::ProtocolViolation {
                    capability: EXECUTION_CLASS,
                }
            })?;
            let request_id = self.next_id.fetch_add(1, Ordering::Relaxed);
            if request_id == 0 {
                return Err(RuntimeFailure::Internal {
                    detail: "Remote request identity exhausted".to_owned(),
                });
            }
            let state = Arc::new(AtomicU8::new(REMOTE_WORK_QUEUED));
            let body = serde_json::to_vec(&InvokeRequest {
                protocol: PROTOCOL_VERSION,
                generation_id: &self.generation_id,
                request_id,
                capability: &capability,
                operation: &operation,
                request: request.as_ref(),
            })
            .map_err(remote_internal)?;
            {
                let mut pending = self.pending.lock().expect("remote pending");
                if self.stopped.load(Ordering::Acquire) {
                    return Err(RuntimeFailure::PluginFailure {
                        detail: "Remote Plugin generation is unavailable".to_owned(),
                    });
                }
                if pending.len() >= self.limits.max_pending_requests {
                    return Err(RuntimeFailure::ResourceExhausted {
                        capability: EXECUTION_CLASS,
                        operation,
                    });
                }
                pending.insert(request_id, Arc::clone(&state));
            }
            let (sender, receiver) = futures::channel::oneshot::channel();
            if !self.workers.invoke(RemoteInvokeWork {
                request_id,
                context_request_id: context.request_id(),
                body,
                state: Arc::clone(&state),
                queued_at: Instant::now(),
                outcome: sender,
            }) {
                self.pending
                    .lock()
                    .expect("remote pending")
                    .remove(&request_id);
                return Err(RuntimeFailure::ResourceExhausted {
                    capability: EXECUTION_CLASS,
                    operation,
                });
            }
            let mut pending_guard = RemotePendingGuard::new(self.clone(), request_id, state);

            let cancellation = context.cancellation();
            let mut response = receiver.fuse();
            let mut cancelled = cancellation.cancelled().fuse();
            select! {
                outcome = response => {
                    pending_guard.disarm();
                    outcome.unwrap_or_else(|_| Err(RuntimeFailure::PluginFailure {
                        detail: "Remote Plugin response channel closed".to_owned(),
                    }))
                },
                () = cancelled => {
                    Err(RuntimeFailure::Cancelled { request_id: context.request_id() })
                }
            }
        })
    }
}

impl Drop for RemoteGeneration {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug)]
struct RemoteLifecycle {
    generation: Rc<RemoteGeneration>,
}

impl PluginLifecycle for RemoteLifecycle {
    fn deactivate(&self, _: lenso_kernel::DeactivateContext) -> lenso_kernel::PluginFuture {
        let Some(job) = self.generation.begin_stop() else {
            return Box::pin(futures::future::ready(Ok(())));
        };
        let job = Arc::new(Mutex::new(Some(job)));
        let worker_job = Arc::clone(&job);
        let (sender, receiver) = futures::channel::oneshot::channel();
        match thread::Builder::new()
            .name("lenso-remote-shutdown".to_owned())
            .spawn(move || {
                if let Some(job) = worker_job.lock().expect("remote shutdown job").take() {
                    job.run();
                }
                let _ = sender.send(());
            }) {
            Ok(_) => Box::pin(async move {
                receiver.await.map_err(|_| RuntimeFailure::Internal {
                    detail: "Remote shutdown worker stopped before completion".to_owned(),
                })
            }),
            Err(error) => {
                if let Some(job) = job.lock().expect("remote shutdown job").take() {
                    job.run();
                }
                Box::pin(futures::future::ready(Err(remote_internal(error))))
            }
        }
    }
}

fn parse_base_url(encoded: &str) -> Result<Url, RuntimeFailure> {
    let mut url = Url::parse(encoded).map_err(invalid_url)?;
    if url.cannot_be_a_base()
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return invalid(
            "Remote base_url must be an origin/path without credentials, query, or fragment",
        );
    }
    match url.scheme() {
        "https" => {}
        "http" if loopback_host(url.host_str().expect("validated host")) => {}
        _ => return invalid("Remote base_url requires HTTPS except for loopback development"),
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

fn loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn read_json_response<T: DeserializeOwned>(
    mut response: reqwest::blocking::Response,
    limit: usize,
) -> Result<T, RuntimeFailure> {
    let status = response.status();
    if status != StatusCode::OK {
        return Err(RuntimeFailure::PluginFailure {
            detail: format!("Remote Plugin returned HTTP {status}"),
        });
    }
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(remote_resource_exhausted());
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| remote_failure("response read failed", &error))?;
    if bytes.len() > limit {
        return Err(remote_resource_exhausted());
    }
    decode_strict(&bytes).map_err(|_| RuntimeFailure::ProtocolViolation {
        capability: EXECUTION_CLASS,
    })
}

fn decode_strict<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = T::deserialize(&mut deserializer).map_err(|error| error.to_string())?;
    deserializer.end().map_err(|error| error.to_string())?;
    Ok(value)
}

fn decode_invoke_response(
    response: InvokeResponse,
    generation_id: &str,
    request_id: u64,
) -> RemoteResult {
    if response.protocol != PROTOCOL_VERSION
        || response.generation_id != generation_id
        || response.request_id != request_id
    {
        return Err(RuntimeFailure::ProtocolViolation {
            capability: EXECUTION_CLASS,
        });
    }
    match (response.ok, response.error, response.failure) {
        (Some(value), None, None) => Ok(JsonInvocationOutcome::Success(value)),
        (None, Some(value), None) => Ok(JsonInvocationOutcome::DomainError(value)),
        (None, None, Some(detail)) => Err(RuntimeFailure::PluginFailure {
            detail: bounded(detail),
        }),
        _ => Err(RuntimeFailure::ProtocolViolation {
            capability: EXECUTION_CLASS,
        }),
    }
}

fn bounded(mut detail: String) -> String {
    const MAX_DETAIL: usize = 512;
    if detail.len() > MAX_DETAIL {
        let mut boundary = MAX_DETAIL;
        while !detail.is_char_boundary(boundary) {
            boundary -= 1;
        }
        detail.truncate(boundary);
    }
    detail
}

fn invalid<T>(detail: impl Into<String>) -> Result<T, RuntimeFailure> {
    Err(RuntimeFailure::InvalidResolvedPlan {
        detail: detail.into(),
    })
}

fn invalid_url(error: impl std::fmt::Display) -> RuntimeFailure {
    RuntimeFailure::InvalidResolvedPlan {
        detail: format!("invalid Remote base_url: {error}"),
    }
}

fn remote_failure(label: &str, error: &impl std::fmt::Display) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: bounded(format!("Remote Plugin {label}: {error}")),
    }
}

fn remote_internal(error: impl std::fmt::Display) -> RuntimeFailure {
    RuntimeFailure::Internal {
        detail: error.to_string(),
    }
}

fn remote_resource_exhausted() -> RuntimeFailure {
    RuntimeFailure::ResourceExhausted {
        capability: EXECUTION_CLASS,
        operation: "response".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        io::Write as _,
        net::{TcpListener, TcpStream},
        sync::{Arc, Condvar, Mutex, mpsc},
        thread,
        time::{Duration, Instant},
    };

    use super::*;

    #[test]
    #[allow(clippy::too_many_lines)]
    fn shutdown_prioritizes_every_dispatched_invoke_over_a_saturated_cancel_pool() {
        const QUEUED_INVOKES: u64 = 128;
        const PENDING_CAPACITY: usize = 132;
        const PENDING_CAPACITY_U64: u64 = 132;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let (old_cancel_seen, old_cancels) = mpsc::channel();
        let (invoke_seen, invokes) = mpsc::channel();
        let cancelled = Arc::new((Mutex::new(BTreeSet::new()), Condvar::new()));
        let release_old = Arc::new((Mutex::new(false), Condvar::new()));
        let server_cancelled = Arc::clone(&cancelled);
        let server_release_old = Arc::clone(&release_old);
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut handlers = Vec::new();
            while handlers.len() < 10 && Instant::now() < deadline {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let old_cancel_seen = old_cancel_seen.clone();
                        let invoke_seen = invoke_seen.clone();
                        let cancelled = Arc::clone(&server_cancelled);
                        let release_old = Arc::clone(&server_release_old);
                        handlers.push(thread::spawn(move || {
                            let (path, request) = read_test_request(stream.try_clone().unwrap());
                            let request_id = request["request_id"].as_u64().unwrap();
                            match path.as_str() {
                                "/invoke" => {
                                    invoke_seen.send(request_id).unwrap();
                                    let (lock, wake) = &*cancelled;
                                    let cancelled = lock.lock().unwrap();
                                    let (cancelled, wait) = wake
                                        .wait_timeout_while(
                                            cancelled,
                                            Duration::from_secs(3),
                                            |ids| !ids.contains(&request_id),
                                        )
                                        .unwrap();
                                    assert!(!wait.timed_out());
                                    drop(cancelled);
                                    write_test_response(
                                        stream,
                                        &serde_json::json!({
                                            "protocol": PROTOCOL_VERSION,
                                            "generation_id": "shutdown-test",
                                            "request_id": request_id,
                                            "failure": "cancelled"
                                        }),
                                    );
                                }
                                "/cancel" if request_id >= 100 => {
                                    old_cancel_seen.send(request_id).unwrap();
                                    let (lock, wake) = &*release_old;
                                    let released = lock.lock().unwrap();
                                    let (released_guard, wait) = wake
                                        .wait_timeout_while(
                                            released,
                                            Duration::from_secs(3),
                                            |released| !*released,
                                        )
                                        .unwrap();
                                    assert!(!wait.timed_out());
                                    drop(released_guard);
                                    write_test_response(
                                        stream,
                                        &serde_json::json!({"cancelled": true}),
                                    );
                                }
                                "/cancel" => {
                                    let (lock, wake) = &*cancelled;
                                    lock.lock().unwrap().insert(request_id);
                                    wake.notify_all();
                                    write_test_response(
                                        stream,
                                        &serde_json::json!({"cancelled": true}),
                                    );
                                }
                                other => panic!("unexpected shutdown test path {other}"),
                            }
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::yield_now();
                    }
                    Err(error) => panic!("cancel test server failed: {error}"),
                }
            }
            assert_eq!(handlers.len(), 10);
            for handler in handlers {
                handler.join().unwrap();
            }
        });

        let limits = RemoteLimits {
            max_pending_requests: PENDING_CAPACITY,
            request_timeout: Duration::from_secs(30),
            ..RemoteLimits::default()
        };
        let pending = RemotePending::default();
        let uncertain_cancels = RemoteUncertainCancels::default();
        let client = Client::builder().build().unwrap();
        let base = Url::parse(&format!("http://{address}/")).unwrap();
        let workers = RemoteWorkerPool::start(
            client,
            &base.join("invoke").unwrap(),
            base.join("cancel").unwrap(),
            "shutdown-test".to_owned(),
            &pending,
            &uncertain_cancels,
            &limits,
        )
        .unwrap();
        let generation = RemoteGeneration {
            generation_id: "shutdown-test".to_owned(),
            pending,
            uncertain_cancels,
            workers,
            next_id: AtomicU64::new(1),
            stopped: AtomicBool::new(false),
            limits,
        };

        assert!(generation.workers.cancel(100));
        assert!(generation.workers.cancel(101));
        let mut occupied = vec![
            old_cancels.recv_timeout(Duration::from_secs(2)).unwrap(),
            old_cancels.recv_timeout(Duration::from_secs(2)).unwrap(),
        ];
        occupied.sort_unstable();
        assert_eq!(occupied, [100, 101]);

        let mut outcomes = Vec::new();
        for request_id in 1..=4 {
            let state = Arc::new(AtomicU8::new(REMOTE_WORK_QUEUED));
            generation
                .pending
                .lock()
                .unwrap()
                .insert(request_id, Arc::clone(&state));
            let (outcome, receiver) = futures::channel::oneshot::channel();
            outcomes.push(receiver);
            assert!(
                generation.workers.invoke(RemoteInvokeWork {
                    request_id,
                    context_request_id: request_id,
                    body: serde_json::to_vec(&serde_json::json!({
                        "protocol": PROTOCOL_VERSION,
                        "generation_id": "shutdown-test",
                        "request_id": request_id,
                        "capability": "test.echo@1",
                        "operation": "echo",
                        "request": {}
                    }))
                    .unwrap(),
                    state,
                    queued_at: Instant::now(),
                    outcome,
                })
            );
        }
        let mut dispatched = (0..4)
            .map(|_| invokes.recv_timeout(Duration::from_secs(2)).unwrap())
            .collect::<Vec<_>>();
        dispatched.sort_unstable();
        assert_eq!(dispatched, [1, 2, 3, 4]);

        let mut queued_outcomes = Vec::new();
        for request_id in 5..5 + QUEUED_INVOKES {
            let state = Arc::new(AtomicU8::new(REMOTE_WORK_QUEUED));
            generation
                .pending
                .lock()
                .unwrap()
                .insert(request_id, Arc::clone(&state));
            let (outcome, receiver) = futures::channel::oneshot::channel();
            queued_outcomes.push(receiver);
            assert!(
                generation.workers.invoke(RemoteInvokeWork {
                    request_id,
                    context_request_id: request_id,
                    body: serde_json::to_vec(&serde_json::json!({
                        "protocol": PROTOCOL_VERSION,
                        "generation_id": "shutdown-test",
                        "request_id": request_id,
                        "capability": "test.echo@1",
                        "operation": "echo",
                        "request": {}
                    }))
                    .unwrap(),
                    state,
                    queued_at: Instant::now(),
                    outcome,
                })
            );
        }

        for request_id in 200..200 + PENDING_CAPACITY_U64 {
            assert!(generation.workers.cancel(request_id));
        }
        generation.spawn_cancel(1);
        assert!(generation.stopped.load(Ordering::Acquire));
        assert_eq!(
            *generation.uncertain_cancels.lock().unwrap(),
            BTreeSet::from([1, 2, 3, 4])
        );

        let started = Instant::now();
        generation.begin_stop().unwrap().run();
        let elapsed = started.elapsed();
        let (lock, wake) = &*release_old;
        *lock.lock().unwrap() = true;
        wake.notify_all();
        server.join().unwrap();

        assert_eq!(*cancelled.0.lock().unwrap(), BTreeSet::from([1, 2, 3, 4]));
        assert!(generation.pending.lock().unwrap().is_empty());
        for outcome in queued_outcomes {
            assert!(
                futures::executor::block_on(outcome).is_err(),
                "shutdown must drop queued work instead of draining it through invoke workers"
            );
        }
        assert!(
            elapsed < Duration::from_millis(1_500),
            "shutdown must prioritize dispatched work independently of the saturated cancel pool, got {elapsed:?}"
        );
        drop(outcomes);
    }

    fn read_test_request(mut stream: TcpStream) -> (String, serde_json::Value) {
        stream.set_nonblocking(false).unwrap();
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
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(str::trim)
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .unwrap();
        while bytes.len() < header_end + content_length {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0);
            bytes.extend_from_slice(&buffer[..read]);
        }
        let path = headers.split_whitespace().nth(1).unwrap().to_owned();
        let body = serde_json::from_slice(&bytes[header_end..header_end + content_length]).unwrap();
        (path, body)
    }

    fn write_test_response(mut stream: TcpStream, value: &serde_json::Value) {
        let body = serde_json::to_vec(value).unwrap();
        let response = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .and_then(|()| stream.write_all(&body))
        .and_then(|()| stream.flush());
        let _ = response;
    }
}
