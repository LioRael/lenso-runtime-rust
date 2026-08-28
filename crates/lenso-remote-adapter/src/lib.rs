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
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
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
use reqwest::{StatusCode, Url, blocking::Client};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

/// Stable remote HTTP execution class.
pub const EXECUTION_CLASS: &str = "lenso.remote@1";
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

    /// Installs a product-owned client for proxy, identity, or mTLS policy.
    /// Per-request Adapter timeouts remain authoritative.
    #[must_use]
    pub fn with_http_client(mut self, client: Client) -> Self {
        self.client = Some(client);
        self
    }

    fn prepare_instance(
        &self,
        instance: &PluginInstancePlan,
    ) -> Result<PreparedNativePlugin, RuntimeFailure> {
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
    request: Value,
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
    client: Client,
    invoke_url: Url,
    cancel_url: Url,
    generation_id: String,
    pending: Arc<Mutex<BTreeSet<u64>>>,
    workers: Mutex<Vec<thread::JoinHandle<()>>>,
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
        Ok(Rc::new(Self {
            client,
            invoke_url,
            cancel_url,
            generation_id: uuid::Uuid::new_v4().to_string(),
            pending: Arc::new(Mutex::new(BTreeSet::new())),
            workers: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            stopped: AtomicBool::new(false),
            limits,
        }))
    }

    fn track(&self, worker: thread::JoinHandle<()>) {
        let mut workers = self.workers.lock().expect("remote workers");
        let mut index = 0;
        while index < workers.len() {
            if workers[index].is_finished() {
                let worker = workers.swap_remove(index);
                let _ = worker.join();
            } else {
                index += 1;
            }
        }
        workers.push(worker);
    }

    fn spawn_cancel(&self, request_id: u64) {
        let client = self.client.clone();
        let url = self.cancel_url.clone();
        let generation_id = self.generation_id.clone();
        let timeout = self.limits.request_timeout;
        if let Ok(worker) = thread::Builder::new()
            .name("lenso-remote-cancel".to_owned())
            .spawn(move || {
                let _ = client
                    .post(url)
                    .timeout(timeout)
                    .json(&CancelRequest {
                        protocol: PROTOCOL_VERSION,
                        generation_id: &generation_id,
                        request_id,
                    })
                    .send();
            })
        {
            self.track(worker);
        }
    }

    fn stop(&self) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        let pending = self
            .pending
            .lock()
            .expect("remote pending")
            .iter()
            .copied()
            .collect::<Vec<_>>();
        for request_id in pending {
            self.spawn_cancel(request_id);
        }
        let workers = std::mem::take(&mut *self.workers.lock().expect("remote workers"));
        for worker in workers {
            let _ = worker.join();
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
            let request = serde_json::from_str::<Value>(&request_json).map_err(|_| {
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
            {
                let mut pending = self.pending.lock().expect("remote pending");
                if pending.len() >= self.limits.max_pending_requests {
                    return Err(RuntimeFailure::ResourceExhausted {
                        capability: EXECUTION_CLASS,
                        operation,
                    });
                }
                pending.insert(request_id);
            }
            let (sender, receiver) = futures::channel::oneshot::channel();
            let client = self.client.clone();
            let url = self.invoke_url.clone();
            let generation_id = self.generation_id.clone();
            let expected_generation_id = generation_id.clone();
            let pending = self.pending.clone();
            let timeout = self.limits.request_timeout;
            let max_response_bytes = self.limits.max_response_bytes;
            let worker = thread::Builder::new()
                .name("lenso-remote-invoke".to_owned())
                .spawn(move || {
                    let result = client
                        .post(url)
                        .timeout(timeout)
                        .json(&InvokeRequest {
                            protocol: PROTOCOL_VERSION,
                            generation_id: &generation_id,
                            request_id,
                            capability: &capability,
                            operation: &operation,
                            request,
                        })
                        .send()
                        .map_err(|error| remote_failure("invocation request failed", &error))
                        .and_then(|response| {
                            read_json_response::<InvokeResponse>(response, max_response_bytes)
                        })
                        .and_then(|response| {
                            decode_invoke_response(response, &expected_generation_id, request_id)
                        });
                    pending.lock().expect("remote pending").remove(&request_id);
                    let _ = sender.send(result);
                })
                .map_err(remote_internal);
            let worker = match worker {
                Ok(worker) => worker,
                Err(error) => {
                    self.pending
                        .lock()
                        .expect("remote pending")
                        .remove(&request_id);
                    return Err(error);
                }
            };
            self.track(worker);

            let cancellation = context.cancellation();
            let mut response = receiver.fuse();
            let mut cancelled = cancellation.cancelled().fuse();
            select! {
                outcome = response => outcome.unwrap_or_else(|_| Err(RuntimeFailure::PluginFailure {
                    detail: "Remote Plugin response channel closed".to_owned(),
                })),
                () = cancelled => {
                    self.spawn_cancel(request_id);
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
        self.generation.stop();
        Box::pin(futures::future::ready(Ok(())))
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
        detail.truncate(MAX_DETAIL);
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
