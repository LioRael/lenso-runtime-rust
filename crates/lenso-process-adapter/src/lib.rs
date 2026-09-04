//! Trusted child-process Execution Adapter for immutable Plugin Artifacts.
//!
//! The Adapter owns process spawning, bounded framed stdio, exact descriptor
//! readiness, cancellation retirement, and cleanup. It intentionally exposes
//! request-only V1 first; Stream and Host imports fail before readiness.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, BufRead as _, BufReader, BufWriter, Write as _},
    process::{Child, ChildStdin, Command, Stdio},
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
use lenso_process_sdk::PROTOCOL_VERSION;
use lenso_runtime_codec::{
    ArtifactCatalog, ArtifactHandle, JsonCapabilityCodec, JsonInvocationOutcome,
    JsonRequestTransport, codecs_for_instance, json_request_endpoints, prepare_request_app,
    validate_json_plugin_descriptor,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Stable generic process execution class.
pub const EXECUTION_CLASS: &str = "lenso.process@1";
/// Legacy request-only Process protocol accepted by this Adapter path.
pub const RUNTIME_PROFILE_V1: &str = "lenso.process@1";

/// Host-owned resource limits for one Process Plugin generation.
#[derive(Clone, Debug)]
pub struct ProcessLimits {
    pub max_frame_bytes: usize,
    pub max_pending_requests: usize,
    pub startup_timeout: Duration,
}

impl Default for ProcessLimits {
    fn default() -> Self {
        Self {
            max_frame_bytes: lenso_process_sdk::DEFAULT_MAX_FRAME_BYTES,
            max_pending_requests: 64,
            startup_timeout: Duration::from_secs(5),
        }
    }
}

/// Adapter configured with exact admitted Process Artifacts and generated codecs.
#[derive(Debug)]
pub struct ProcessAdapter {
    artifacts: ArtifactCatalog,
    codecs: BTreeMap<String, Rc<dyn JsonCapabilityCodec>>,
    duplicate_codecs: BTreeSet<String>,
    limits: ProcessLimits,
}

impl ProcessAdapter {
    pub fn new(artifacts: ArtifactCatalog) -> Self {
        Self {
            artifacts,
            codecs: BTreeMap::new(),
            duplicate_codecs: BTreeSet::new(),
            limits: ProcessLimits::default(),
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
    pub fn with_limits(mut self, limits: ProcessLimits) -> Self {
        self.limits = limits;
        self
    }

    fn prepare_instance(
        &self,
        instance: &PluginInstancePlan,
    ) -> Result<PreparedNativePlugin, RuntimeFailure> {
        if instance.runtime_profile() != RUNTIME_PROFILE_V1 {
            return invalid(format!(
                "Process Adapter does not support runtime profile `{}`",
                instance.runtime_profile()
            ));
        }
        if instance.entrypoint() != "plugin" {
            return invalid(format!(
                "Process Plugin Instance `{}` needs entrypoint `plugin`",
                instance.instance_key()
            ));
        }
        if !instance.required_capabilities().is_empty()
            || instance
                .provided_capabilities()
                .iter()
                .any(|capability| !capability.stream_operations().is_empty())
        {
            return invalid("Process V1 supports request-only providers without Host imports");
        }
        if !self.duplicate_codecs.is_empty() {
            return invalid(format!(
                "duplicate generated codecs registered for {:?}",
                self.duplicate_codecs
            ));
        }
        let artifact = self.artifacts.require(instance.instance_key())?.clone();
        let codecs = codecs_for_instance(instance, &self.codecs)?;
        let generation = ProcessGeneration::start(artifact, instance, self.limits.clone())?;
        let endpoints = json_request_endpoints(generation.clone(), codecs);
        Ok(PreparedNativePlugin::with_endpoints(
            endpoints,
            Vec::new(),
            ProcessLifecycle { generation },
        ))
    }
}

impl ExecutionAdapter for ProcessAdapter {
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
            return invalid(format!("Instance `{instance_key}` is not a Process Plugin"));
        }
        self.prepare_instance(instance)
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum HostFrame<'a> {
    Invoke {
        id: u64,
        capability: &'a str,
        operation: &'a str,
        request: Value,
    },
    #[expect(
        dead_code,
        reason = "retained for process wire compatibility; abandonment retires the generation"
    )]
    Cancel {
        id: u64,
    },
    Shutdown,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum GuestFrame {
    Ready {
        protocol: String,
        descriptor: Value,
    },
    Result {
        id: u64,
        #[serde(default)]
        ok: Option<Value>,
        #[serde(default)]
        error: Option<Value>,
        #[serde(default)]
        failure: Option<String>,
    },
}

type Pending = Arc<Mutex<BTreeMap<u64, futures::channel::oneshot::Sender<ProcessResult>>>>;
type ProcessResult = Result<JsonInvocationOutcome, RuntimeFailure>;

struct ProcessGeneration {
    _artifact: ArtifactHandle,
    writer: Arc<Mutex<BufWriter<ChildStdin>>>,
    child: Arc<Mutex<Option<Child>>>,
    reader: Mutex<Option<thread::JoinHandle<()>>>,
    pending: Pending,
    next_id: AtomicU64,
    failed: Arc<AtomicBool>,
    stopped: AtomicBool,
    limits: ProcessLimits,
}

impl std::fmt::Debug for ProcessGeneration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessGeneration")
            .field("failed", &self.failed)
            .field("stopped", &self.stopped)
            .finish_non_exhaustive()
    }
}

impl ProcessGeneration {
    #[allow(clippy::too_many_lines)]
    fn start(
        artifact: ArtifactHandle,
        instance: &PluginInstancePlan,
        limits: ProcessLimits,
    ) -> Result<Rc<Self>, RuntimeFailure> {
        let executable = artifact.path().to_path_buf();
        let mut command = Command::new(&executable);
        command
            .env_clear()
            .current_dir(
                artifact
                    .source_path()
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new(".")),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| RuntimeFailure::PluginFailure {
                detail: if error.kind() == io::ErrorKind::PermissionDenied {
                    format!(
                        "failed to start Process Plugin from the stable Artifact path: {error}; \
                         configure an executable Host Artifact staging root"
                    )
                } else {
                    format!("failed to start Process Plugin: {error}")
                },
            })?;
        let stdin = child.stdin.take().ok_or_else(|| RuntimeFailure::Internal {
            detail: "Process Plugin stdin was not piped".to_owned(),
        })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RuntimeFailure::Internal {
                detail: "Process Plugin stdout was not piped".to_owned(),
            })?;
        if let Some(stderr) = child.stderr.take() {
            thread::Builder::new()
                .name("lenso-process-stderr".to_owned())
                .spawn(move || {
                    for line in BufReader::new(stderr).lines() {
                        if line.is_err() {
                            break;
                        }
                    }
                })
                .map_err(internal)?;
        }

        let writer = Arc::new(Mutex::new(BufWriter::new(stdin)));
        let pending = Pending::default();
        let failed = Arc::new(AtomicBool::new(false));
        let (ready_sender, ready_receiver) = std::sync::mpsc::sync_channel(1);
        let reader_pending = pending.clone();
        let reader_failed = failed.clone();
        let max_frame_bytes = limits.max_frame_bytes;
        let reader = thread::Builder::new()
            .name("lenso-process-reader".to_owned())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                let ready = read_guest_frame(&mut reader, max_frame_bytes).and_then(|frame| {
                    let GuestFrame::Ready {
                        protocol,
                        descriptor,
                    } = frame
                    else {
                        return Err("Process Plugin did not send readiness first".to_owned());
                    };
                    if protocol != PROTOCOL_VERSION {
                        return Err(format!("unsupported Process protocol `{protocol}`"));
                    }
                    Ok(descriptor)
                });
                if ready_sender.send(ready).is_err() {
                    return;
                }
                loop {
                    match read_guest_frame(&mut reader, max_frame_bytes) {
                        Ok(GuestFrame::Result {
                            id,
                            ok,
                            error,
                            failure,
                        }) => {
                            let outcome = decode_result(ok, error, failure);
                            if let Some(sender) =
                                reader_pending.lock().expect("pending").remove(&id)
                            {
                                let _ = sender.send(outcome);
                            } else {
                                retire_pending(
                                    &reader_pending,
                                    &reader_failed,
                                    "Process Plugin returned an unknown request id",
                                );
                                return;
                            }
                        }
                        Ok(GuestFrame::Ready { .. }) => {
                            retire_pending(
                                &reader_pending,
                                &reader_failed,
                                "Process Plugin sent duplicate readiness",
                            );
                            return;
                        }
                        Err(detail) => {
                            retire_pending(&reader_pending, &reader_failed, &detail);
                            return;
                        }
                    }
                }
            })
            .map_err(internal)?;

        let descriptor = match ready_receiver.recv_timeout(limits.startup_timeout) {
            Ok(Ok(descriptor)) => descriptor,
            Ok(Err(detail)) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(RuntimeFailure::PluginFailure { detail });
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(RuntimeFailure::PluginFailure {
                    detail: "Process Plugin readiness timed out".to_owned(),
                });
            }
        };
        let encoded =
            serde_json::to_string(&descriptor).map_err(|error| RuntimeFailure::PluginFailure {
                detail: format!("Process descriptor is not JSON: {error}"),
            })?;
        validate_json_plugin_descriptor(instance, &encoded)?;

        Ok(Rc::new(Self {
            _artifact: artifact,
            writer,
            child: Arc::new(Mutex::new(Some(child))),
            reader: Mutex::new(Some(reader)),
            pending,
            next_id: AtomicU64::new(1),
            failed,
            stopped: AtomicBool::new(false),
            limits,
        }))
    }

    fn send(&self, frame: &HostFrame<'_>) -> Result<(), RuntimeFailure> {
        let bytes = serde_json::to_vec(frame).map_err(|_| protocol_failure())?;
        if bytes.len() >= self.limits.max_frame_bytes {
            return Err(RuntimeFailure::ResourceExhausted {
                capability: EXECUTION_CLASS,
                operation: "frame".to_owned(),
            });
        }
        let mut writer = self.writer.lock().expect("process writer");
        writer
            .write_all(&bytes)
            .and_then(|()| writer.write_all(b"\n"))
            .and_then(|()| writer.flush())
            .map_err(|error| process_io(&error))
    }

    fn stop(&self) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.send(&HostFrame::Shutdown);
        if let Some(mut child) = self.child.lock().expect("process child").take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.lock().expect("process reader").take() {
            let _ = reader.join();
        }
        retire_pending(&self.pending, &self.failed, "Process Plugin stopped");
    }

    fn abort(&self) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        self.failed.store(true, Ordering::Release);
        if let Some(mut child) = self.child.lock().expect("process child").take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.lock().expect("process reader").take() {
            let _ = reader.join();
        }
        retire_pending(
            &self.pending,
            &self.failed,
            "Process Plugin invocation was abandoned",
        );
    }
}

struct PendingInvocationGuard {
    generation: Rc<ProcessGeneration>,
    id: u64,
    armed: bool,
}

impl PendingInvocationGuard {
    fn new(generation: Rc<ProcessGeneration>, id: u64) -> Self {
        Self {
            generation,
            id,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingInvocationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let was_pending = self
            .generation
            .pending
            .lock()
            .expect("pending")
            .remove(&self.id)
            .is_some();
        if was_pending {
            self.generation.abort();
        }
    }
}

impl JsonRequestTransport for ProcessGeneration {
    fn invoke(
        self: Rc<Self>,
        capability: String,
        operation: String,
        request_json: String,
        context: InvocationContext,
    ) -> futures::future::LocalBoxFuture<'static, ProcessResult> {
        Box::pin(async move {
            if self.failed.load(Ordering::Acquire) || self.stopped.load(Ordering::Acquire) {
                return Err(RuntimeFailure::PluginFailure {
                    detail: "Process Plugin generation is unavailable".to_owned(),
                });
            }
            let request = serde_json::from_str::<Value>(&request_json).map_err(|_| {
                RuntimeFailure::ProtocolViolation {
                    capability: EXECUTION_CLASS,
                }
            })?;
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            if id == 0 {
                self.failed.store(true, Ordering::Release);
                return Err(RuntimeFailure::PluginFailure {
                    detail: "Process request identity exhausted".to_owned(),
                });
            }
            let (sender, receiver) = futures::channel::oneshot::channel();
            {
                let mut pending = self.pending.lock().expect("pending");
                if pending.len() >= self.limits.max_pending_requests {
                    return Err(RuntimeFailure::ResourceExhausted {
                        capability: EXECUTION_CLASS,
                        operation,
                    });
                }
                pending.insert(id, sender);
            }
            if let Err(error) = self.send(&HostFrame::Invoke {
                id,
                capability: &capability,
                operation: &operation,
                request,
            }) {
                self.pending.lock().expect("pending").remove(&id);
                return Err(error);
            }
            let mut pending_guard = PendingInvocationGuard::new(self.clone(), id);
            let cancellation = context.cancellation();
            let mut response = receiver.fuse();
            let mut cancelled = cancellation.cancelled().fuse();
            select! {
                outcome = response => {
                    pending_guard.disarm();
                    outcome.unwrap_or_else(|_| Err(RuntimeFailure::PluginFailure {
                        detail: "Process Plugin response channel closed".to_owned(),
                    }))
                },
                () = cancelled => {
                    Err(RuntimeFailure::Cancelled { request_id: context.request_id() })
                }
            }
        })
    }
}

impl Drop for ProcessGeneration {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug)]
struct ProcessLifecycle {
    generation: Rc<ProcessGeneration>,
}

impl PluginLifecycle for ProcessLifecycle {
    fn deactivate(&self, _: lenso_kernel::DeactivateContext) -> lenso_kernel::PluginFuture {
        self.generation.stop();
        Box::pin(futures::future::ready(Ok(())))
    }
}

fn decode_result(
    ok: Option<Value>,
    error: Option<Value>,
    failure: Option<String>,
) -> ProcessResult {
    match (ok, error, failure) {
        (Some(value), None, None) => Ok(JsonInvocationOutcome::Success(value)),
        (None, Some(value), None) => Ok(JsonInvocationOutcome::DomainError(value)),
        (None, None, Some(detail)) => Err(RuntimeFailure::PluginFailure {
            detail: bounded(detail),
        }),
        _ => Err(protocol_failure()),
    }
}

fn read_guest_frame(reader: &mut impl io::BufRead, limit: usize) -> Result<GuestFrame, String> {
    let mut bytes = Vec::new();
    let read = io::Read::take(
        &mut *reader,
        u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1),
    )
    .read_until(b'\n', &mut bytes)
    .map_err(|error| format!("failed to read Process frame: {error}"))?;
    if read == 0 {
        return Err("Process Plugin exited".to_owned());
    }
    if bytes.len() > limit || !bytes.ends_with(b"\n") {
        return Err("Process Plugin frame exceeds the configured limit".to_owned());
    }
    bytes.pop();
    serde_json::from_slice(&bytes).map_err(|error| format!("invalid Process frame: {error}"))
}

fn retire_pending(pending: &Pending, failed: &AtomicBool, detail: &str) {
    failed.store(true, Ordering::Release);
    let senders = std::mem::take(&mut *pending.lock().expect("pending"));
    for (_, sender) in senders {
        let _ = sender.send(Err(RuntimeFailure::PluginFailure {
            detail: bounded(detail.to_owned()),
        }));
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

fn internal(error: impl std::fmt::Display) -> RuntimeFailure {
    RuntimeFailure::Internal {
        detail: error.to_string(),
    }
}

fn process_io(error: &io::Error) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: format!("Process Plugin I/O failed: {error}"),
    }
}

fn protocol_failure() -> RuntimeFailure {
    RuntimeFailure::ProtocolViolation {
        capability: EXECUTION_CLASS,
    }
}
