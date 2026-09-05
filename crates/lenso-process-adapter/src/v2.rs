use std::{
    collections::BTreeMap,
    io::{self, BufRead as _, BufReader, BufWriter, Write as _},
    process::{Child, ChildStdin, Stdio},
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures::{FutureExt as _, StreamExt as _, channel::mpsc, select};
use lenso_app_plan::{CapabilityCardinality, PluginInstancePlan};
use lenso_kernel::{
    ActivateContext, CancellationToken, DeactivateContext, InvocationContext, PluginDependencies,
    PluginLifecycle, PreparedNativePlugin, RuntimeFailure,
};
use lenso_process_protocol::{
    VALUE_PROFILE,
    authoring::{
        AuthoringLimits, CancelAck, CancelParams, FactoryOutcome, InitializeParams,
        InvocationOutcome, InvocationResult, InvocationScope, OutboundCallParams, ProvidedEndpoint,
        RequirementCardinality, RequirementDeclaration, RouteDescriptor,
        RuntimeFailure as WireFailure, SessionIdentity, Settlement, SettlementState,
        StopHookOutcome, StopParams,
    },
};
use lenso_process_sdk::{GuestFrameV2, HostFrameV2};
use lenso_runtime_codec::{
    ArtifactCatalog, ArtifactHandle, JsonCapabilityCodec, JsonHostImports, JsonInvocationOutcome,
    JsonRequestTransport, codecs_for_instance, codecs_for_requirements, json_request_endpoints,
};
use serde::de::DeserializeOwned;
use sha2::{Digest as _, Sha256};

use super::{ProcessLauncher, ProcessLimits, invalid};

static NEXT_PROCESS_SESSION: AtomicU64 = AtomicU64::new(1);

type LifecycleSender<T> = futures::channel::oneshot::Sender<Result<T, RuntimeFailure>>;
type LifecycleSlot<T> = Arc<Mutex<Option<LifecycleSender<T>>>>;
type InvocationSender = mpsc::UnboundedSender<InvocationEvent>;
type PendingInvocations = Arc<Mutex<BTreeMap<String, InvocationSender>>>;

#[derive(Debug)]
enum InvocationEvent {
    Outcome(InvocationResult),
    Outbound(OutboundCallParams),
    CancelAck(CancelAck),
    Settlement(Settlement),
}

#[derive(Clone, Debug)]
struct EndpointIdentity {
    endpoint_id: String,
    descriptor_version: String,
    descriptor_digest: String,
}

#[derive(Clone, Debug)]
struct ProcessExecutionProfile {
    execution_class: &'static str,
    runtime_profile: &'static str,
    launcher: ProcessLauncher,
}

pub(super) fn prepare_instance(
    artifacts: &ArtifactCatalog,
    codecs: &BTreeMap<String, Rc<dyn JsonCapabilityCodec>>,
    instance: &PluginInstancePlan,
    limits: ProcessLimits,
    execution_class: &'static str,
    runtime_profile: &'static str,
    launcher: ProcessLauncher,
) -> Result<PreparedNativePlugin, RuntimeFailure> {
    if instance.entrypoint() != "plugin" {
        return invalid(format!(
            "Process Plugin Instance `{}` needs entrypoint `plugin`",
            instance.instance_key()
        ));
    }
    if instance.provided_capabilities().iter().any(|capability| {
        !capability.stream_operations().is_empty() || !capability.event_operations().is_empty()
    }) {
        return invalid("Process V2 currently supports Request Capability endpoints");
    }
    let provided = codecs_for_instance(instance, codecs)?;
    let required = codecs_for_requirements(instance, codecs)?;
    for codec in provided.iter().chain(&required) {
        validate_digest(codec.descriptor_digest(), codec.capability_id())?;
    }
    let imports = Rc::new(JsonHostImports::new(required.clone(), 0)?);
    let artifact = artifacts.require(instance.instance_key())?.clone();
    let profile = ProcessExecutionProfile {
        execution_class,
        runtime_profile,
        launcher,
    };
    let generation = ProcessGenerationV2::start(
        artifact,
        instance.clone(),
        provided.clone(),
        required,
        imports,
        limits,
        &profile,
    )?;
    let endpoints = json_request_endpoints(generation.clone(), provided);
    Ok(PreparedNativePlugin::with_endpoints(
        endpoints,
        Vec::new(),
        ProcessLifecycleV2 { generation },
    ))
}

struct ProcessGenerationV2 {
    _artifact: ArtifactHandle,
    instance: PluginInstancePlan,
    provided: Vec<Rc<dyn JsonCapabilityCodec>>,
    required: Vec<Rc<dyn JsonCapabilityCodec>>,
    endpoints: BTreeMap<String, EndpointIdentity>,
    imports: Rc<JsonHostImports>,
    writer: Arc<Mutex<BufWriter<ChildStdin>>>,
    child: Arc<Mutex<Option<Child>>>,
    reader: Mutex<Option<thread::JoinHandle<()>>>,
    initialized: LifecycleSlot<InitializeParams>,
    constructed: LifecycleSlot<lenso_process_protocol::authoring::ConstructedResult>,
    stopped_result: LifecycleSlot<lenso_process_protocol::authoring::StoppedResult>,
    pending: PendingInvocations,
    failed: Arc<AtomicBool>,
    stop_started: AtomicBool,
    stopped: AtomicBool,
    limits: ProcessLimits,
    execution_class: &'static str,
    identity: SessionIdentity,
    initialization: std::cell::RefCell<Option<InitializeParams>>,
}

impl std::fmt::Debug for ProcessGenerationV2 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessGenerationV2")
            .field("instance", &self.instance.instance_key())
            .field("failed", &self.failed)
            .field("stopped", &self.stopped)
            .finish_non_exhaustive()
    }
}

impl ProcessGenerationV2 {
    #[expect(
        clippy::too_many_lines,
        reason = "process creation keeps every owned pipe and cleanup transfer in one transaction"
    )]
    fn start(
        artifact: ArtifactHandle,
        instance: PluginInstancePlan,
        provided: Vec<Rc<dyn JsonCapabilityCodec>>,
        required: Vec<Rc<dyn JsonCapabilityCodec>>,
        imports: Rc<JsonHostImports>,
        limits: ProcessLimits,
        profile: &ProcessExecutionProfile,
    ) -> Result<Rc<Self>, RuntimeFailure> {
        let generation = NEXT_PROCESS_SESSION.fetch_add(1, Ordering::Relaxed);
        if generation == 0 {
            return Err(RuntimeFailure::ResourceExhausted {
                capability: profile.execution_class,
                operation: "session".to_owned(),
            });
        }
        let mut command = profile.launcher.command(artifact.path());
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
                detail: format!("failed to start Process V2 Plugin: {error}"),
            })?;
        let Some(stdin) = child.stdin.take() else {
            terminate_child(&mut child);
            return Err(RuntimeFailure::Internal {
                detail: "Process V2 stdin was not piped".to_owned(),
            });
        };
        let Some(stdout) = child.stdout.take() else {
            terminate_child(&mut child);
            return Err(RuntimeFailure::Internal {
                detail: "Process V2 stdout was not piped".to_owned(),
            });
        };
        if let Some(stderr) = child.stderr.take()
            && let Err(error) = thread::Builder::new()
                .name("lenso-process-v2-stderr".to_owned())
                .spawn(move || {
                    for line in BufReader::new(stderr).lines() {
                        if line.is_err() {
                            break;
                        }
                    }
                })
        {
            terminate_child(&mut child);
            return Err(internal(error));
        }

        let initialized = LifecycleSlot::default();
        let constructed = LifecycleSlot::default();
        let stopped_result = LifecycleSlot::default();
        let pending = PendingInvocations::default();
        let failed = Arc::new(AtomicBool::new(false));
        let reader = match spawn_reader(
            stdout,
            initialized.clone(),
            constructed.clone(),
            stopped_result.clone(),
            pending.clone(),
            failed.clone(),
            limits.max_frame_bytes,
        ) {
            Ok(reader) => reader,
            Err(error) => {
                terminate_child(&mut child);
                return Err(error);
            }
        };
        let identity = SessionIdentity {
            session: format!("process-{}-{generation}", std::process::id()),
            plugin_instance: instance.instance_key().to_owned(),
            plugin_generation: generation.to_string(),
            artifact_digest: artifact.digest().to_owned(),
            contract_digest: contract_digest(&instance, &provided, &required),
            runtime_profile: profile.runtime_profile.to_owned(),
            value_profile: VALUE_PROFILE.to_owned(),
        };
        let endpoints = provided
            .iter()
            .enumerate()
            .map(|(index, codec)| {
                (
                    codec.capability_id().to_owned(),
                    EndpointIdentity {
                        endpoint_id: format!("endpoint-{index}"),
                        descriptor_version: codec.descriptor_version().to_owned(),
                        descriptor_digest: codec.descriptor_digest().to_owned(),
                    },
                )
            })
            .collect();
        Ok(Rc::new(Self {
            _artifact: artifact,
            instance,
            provided,
            required,
            endpoints,
            imports,
            writer: Arc::new(Mutex::new(BufWriter::new(stdin))),
            child: Arc::new(Mutex::new(Some(child))),
            reader: Mutex::new(Some(reader)),
            initialized,
            constructed,
            stopped_result,
            pending,
            failed,
            stop_started: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            limits,
            execution_class: profile.execution_class,
            identity,
            initialization: std::cell::RefCell::new(None),
        }))
    }

    fn initialize(
        &self,
        dependencies: &PluginDependencies,
    ) -> Result<InitializeParams, RuntimeFailure> {
        self.imports.activate(dependencies)?;
        let bindings = self.imports.descriptors()?;
        let mut orders = BTreeMap::<String, u32>::new();
        let mut routes = bindings
            .into_iter()
            .map(|binding| {
                let order = orders.entry(binding.requirement_id.clone()).or_default();
                let route = RouteDescriptor {
                    route_id: format!("route-{}", binding.binding_id),
                    requirement_id: binding.requirement_id,
                    capability_id: binding.capability_id,
                    descriptor_version: binding.descriptor_version,
                    descriptor_digest: binding.descriptor_digest,
                    provider_instance: binding.provider_instance,
                    provider_order: *order,
                };
                *order += 1;
                route
            })
            .collect::<Vec<_>>();
        routes.sort_by(|left, right| {
            (
                &left.requirement_id,
                left.provider_order,
                &left.provider_instance,
            )
                .cmp(&(
                    &right.requirement_id,
                    right.provider_order,
                    &right.provider_instance,
                ))
        });
        let mut required_declarations = self
            .instance
            .required_capabilities()
            .iter()
            .map(|requirement| {
                let codec = self
                    .required
                    .iter()
                    .find(|codec| codec.capability_id() == requirement.capability_id())
                    .expect("required codecs were validated during preparation");
                RequirementDeclaration {
                    requirement_id: requirement.requirement_id().to_owned(),
                    capability_id: requirement.capability_id().to_owned(),
                    descriptor_version: requirement.descriptor_version().to_owned(),
                    descriptor_digest: codec.descriptor_digest().to_owned(),
                    cardinality: cardinality(requirement.cardinality()),
                }
            })
            .collect::<Vec<_>>();
        required_declarations.sort_by(|left, right| left.requirement_id.cmp(&right.requirement_id));
        let mut provided_endpoints = self
            .provided
            .iter()
            .map(|codec| {
                let endpoint = &self.endpoints[codec.capability_id()];
                ProvidedEndpoint {
                    endpoint_id: endpoint.endpoint_id.clone(),
                    capability_id: codec.capability_id().to_owned(),
                    descriptor_version: endpoint.descriptor_version.clone(),
                    descriptor_digest: endpoint.descriptor_digest.clone(),
                }
            })
            .collect::<Vec<_>>();
        provided_endpoints.sort_by(|left, right| left.endpoint_id.cmp(&right.endpoint_id));
        let max_pending = u32::try_from(self.limits.max_pending_requests)
            .unwrap_or(u32::MAX)
            .min(1_024);
        let initialization = InitializeParams {
            api_version: lenso_process_protocol::authoring::AUTHORING_API_VERSION,
            identity: self.identity.clone(),
            config: serde_json::from_str(self.instance.configuration()).map_err(|error| {
                RuntimeFailure::InvalidResolvedPlan {
                    detail: format!("invalid Process V2 configuration: {error}"),
                }
            })?,
            required_declarations,
            routes,
            provided_endpoints,
            limits: AuthoringLimits {
                max_frame_bytes: self.limits.max_frame_bytes as u64,
                max_active_invocations: max_pending,
                max_active_outbound_calls: max_pending,
                max_queued_calls: max_pending,
                max_unfinished_executions: max_pending,
                max_retired_ids: max_pending.saturating_mul(16).max(1),
            },
        };
        initialization
            .validate()
            .map_err(|error| protocol(self.execution_class, error))?;
        Ok(initialization)
    }

    fn send(&self, frame: &HostFrameV2) -> Result<(), RuntimeFailure> {
        if self.failed.load(Ordering::Acquire) || self.stopped.load(Ordering::Acquire) {
            return Err(unavailable());
        }
        let bytes = serde_json::to_vec(frame).map_err(|error| RuntimeFailure::Internal {
            detail: error.to_string(),
        })?;
        if bytes.len() > self.limits.max_frame_bytes {
            return Err(RuntimeFailure::ResourceExhausted {
                capability: self.execution_class,
                operation: "frame".to_owned(),
            });
        }
        let mut writer = self.writer.lock().expect("Process V2 writer");
        writer
            .write_all(&bytes)
            .and_then(|()| writer.write_all(b"\n"))
            .and_then(|()| writer.flush())
            .map_err(|error| RuntimeFailure::PluginFailure {
                detail: format!("Process V2 I/O failed: {error}"),
            })
    }

    fn abort(&self) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        self.failed.store(true, Ordering::Release);
        if let Some(mut child) = self.child.lock().expect("Process V2 child").take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        retire_all(
            &self.initialized,
            &self.constructed,
            &self.stopped_result,
            &self.pending,
            "Process V2 generation terminated",
        );
    }
}

impl JsonRequestTransport for ProcessGenerationV2 {
    #[expect(
        clippy::too_many_lines,
        reason = "the invocation protocol state machine keeps outcome, outbound call, cancellation and settlement ordering together"
    )]
    fn invoke(
        self: Rc<Self>,
        capability: String,
        operation: String,
        request_json: String,
        context: InvocationContext,
    ) -> futures::future::LocalBoxFuture<'static, Result<JsonInvocationOutcome, RuntimeFailure>>
    {
        Box::pin(async move {
            let initialization = self
                .initialization
                .borrow()
                .clone()
                .ok_or(RuntimeFailure::AdmissionClosed)?;
            let endpoint = self.endpoints.get(&capability).ok_or({
                RuntimeFailure::ProtocolViolation {
                    capability: self.execution_class,
                }
            })?;
            let correlation_id = context.request_id().to_string();
            let scope = invocation_scope(&context, self.execution_class)?;
            let params = lenso_process_protocol::authoring::InvokeParams {
                session: self.identity.session.clone(),
                correlation_id: correlation_id.clone(),
                endpoint_id: endpoint.endpoint_id.clone(),
                capability_id: capability,
                descriptor_version: endpoint.descriptor_version.clone(),
                descriptor_digest: endpoint.descriptor_digest.clone(),
                operation: operation.clone(),
                scope: scope.clone(),
                payload: serde_json::from_str(&request_json).map_err(|_| {
                    RuntimeFailure::ProtocolViolation {
                        capability: self.execution_class,
                    }
                })?,
            };
            params
                .validate_against(&initialization)
                .map_err(|error| protocol(self.execution_class, error))?;
            let (sender, mut receiver) = mpsc::unbounded();
            {
                let mut pending = self.pending.lock().expect("Process V2 pending");
                if pending.len() >= self.limits.max_pending_requests {
                    return Err(RuntimeFailure::ResourceExhausted {
                        capability: self.execution_class,
                        operation,
                    });
                }
                if pending.insert(correlation_id.clone(), sender).is_some() {
                    return Err(protocol_failure(self.execution_class));
                }
            }
            if let Err(error) = self.send(&HostFrameV2::Invoke(params.clone())) {
                self.pending
                    .lock()
                    .expect("Process V2 pending")
                    .remove(&correlation_id);
                return Err(error);
            }
            let mut outcome = None;
            let mut cancellation_sent = false;
            loop {
                let event = receiver.next().fuse();
                let cancelled = if cancellation_sent {
                    futures::future::pending().boxed_local()
                } else {
                    context.cancellation().cancelled().boxed_local()
                }
                .fuse();
                futures::pin_mut!(event, cancelled);
                select! {
                    event = event => match event.ok_or_else(unavailable)? {
                        InvocationEvent::Outcome(result) => {
                            result
                                .validate_for(&params)
                                .map_err(|error| protocol(self.execution_class, error))?;
                            outcome = Some(result.outcome);
                        }
                        InvocationEvent::Outbound(call) => {
                            if call
                                .validate_against(&initialization, &scope, true)
                                .is_err()
                            {
                                self.abort();
                                return Err(protocol_failure(self.execution_class));
                            }
                            let binding_id = match route_binding_id(
                                &initialization,
                                &call,
                                self.execution_class,
                            ) {
                                Ok(binding_id) => binding_id,
                                Err(error) => {
                                    self.abort();
                                    return Err(error);
                                }
                            };
                            let result = self.imports.invoke(
                                binding_id,
                                call.operation.clone(),
                                call.payload.clone(),
                                context.clone(),
                            ).await;
                            self.send(&HostFrameV2::OutboundResult(InvocationResult {
                                session: call.session,
                                correlation_id: call.correlation_id,
                                outcome: wire_outcome(result),
                            }))?;
                        }
                        InvocationEvent::CancelAck(ack) => {
                            if !cancellation_sent || !ack.accepted || ack.session != self.identity.session
                                || ack.scope_id != scope.scope_id || ack.correlation_id != correlation_id
                            {
                                return Err(protocol_failure(self.execution_class));
                            }
                        }
                        InvocationEvent::Settlement(settlement) => {
                            settlement
                                .validate_for(&self.identity)
                                .map_err(|error| protocol(self.execution_class, error))?;
                            if settlement.scope_id != scope.scope_id
                                || settlement.correlation_id != correlation_id
                            {
                                return Err(protocol_failure(self.execution_class));
                            }
                            self.pending.lock().expect("Process V2 pending").remove(&correlation_id);
                            if cancellation_sent && settlement.state != SettlementState::Completed {
                                return Err(RuntimeFailure::Cancelled { request_id: context.request_id() });
                            }
                            return outcome
                                .take()
                                .ok_or_else(|| protocol_failure(self.execution_class))
                                .and_then(|outcome| from_wire_outcome(outcome, self.execution_class));
                        }
                    },
                    () = cancelled => {
                        cancellation_sent = true;
                        self.send(&HostFrameV2::Cancel(CancelParams {
                            session: self.identity.session.clone(),
                            scope_id: scope.scope_id.clone(),
                            correlation_id: correlation_id.clone(),
                            reason: "caller cancelled the invocation".to_owned(),
                        }))?;
                        arm_termination(
                            self.child.clone(),
                            self.pending.clone(),
                            self.failed.clone(),
                            correlation_id.clone(),
                            self.limits.cancellation_settlement_timeout,
                        );
                    }
                }
            }
        })
    }
}

#[derive(Debug)]
struct ProcessLifecycleV2 {
    generation: Rc<ProcessGenerationV2>,
}

impl PluginLifecycle for ProcessLifecycleV2 {
    fn construct(&self, context: ActivateContext) -> lenso_kernel::PluginFuture {
        let generation = self.generation.clone();
        Box::pin(async move {
            let initialization = generation.initialize(context.dependencies())?;
            let (sender, receiver) = futures::channel::oneshot::channel();
            generation
                .initialized
                .lock()
                .expect("initialized slot")
                .replace(sender);
            generation.send(&HostFrameV2::Initialize(initialization.clone()))?;
            let echoed = await_initialization(receiver, &context, &generation).await?;
            initialization
                .validate_initialized(&echoed)
                .map_err(|error| protocol(generation.execution_class, error))?;
            generation
                .initialization
                .replace(Some(initialization.clone()));

            let remaining_budget_nanos = u128::from(u64::MAX).to_string();
            let params = lenso_process_protocol::authoring::ConstructParams {
                session: generation.identity.session.clone(),
                lifecycle_scope_id: "construct-1".to_owned(),
                remaining_budget_nanos,
            };
            let scope = lifecycle_scope(&params.lifecycle_scope_id, &params.remaining_budget_nanos);
            let dependency_context = context
                .dependencies()
                .invocation_context(None, context.cancellation())?;
            let pending_key = lifecycle_pending_key(&scope.scope_id);
            let (event_sender, event_receiver) = mpsc::unbounded();
            if generation
                .pending
                .lock()
                .expect("Process V2 pending")
                .insert(pending_key.clone(), event_sender)
                .is_some()
            {
                return Err(protocol_failure(generation.execution_class));
            }
            let (sender, receiver) = futures::channel::oneshot::channel();
            generation
                .constructed
                .lock()
                .expect("constructed slot")
                .replace(sender);
            generation.send(&HostFrameV2::Construct(params.clone()))?;
            let result = await_lifecycle(
                receiver,
                event_receiver,
                dependency_context,
                context.cancellation(),
                &generation,
                &initialization,
                &scope,
            )
            .await;
            generation
                .pending
                .lock()
                .expect("Process V2 pending")
                .remove(&pending_key);
            let result = result?;
            result
                .validate_for(&params)
                .map_err(|error| protocol(generation.execution_class, error))?;
            match result.outcome {
                FactoryOutcome::Constructed => Ok(()),
                FactoryOutcome::Failed { detail } => Err(RuntimeFailure::PluginFailure { detail }),
            }
        })
    }

    fn deactivate(&self, context: DeactivateContext) -> lenso_kernel::PluginFuture {
        let generation = self.generation.clone();
        Box::pin(async move {
            if generation.stop_started.swap(true, Ordering::AcqRel) {
                return Ok(());
            }
            let params = StopParams {
                session: generation.identity.session.clone(),
                cleanup_scope_id: "cleanup-1".to_owned(),
                remaining_budget_nanos: duration_nanos(context.remaining_budget()),
            };
            let initialization = generation
                .initialization
                .borrow()
                .clone()
                .ok_or(RuntimeFailure::AdmissionClosed)?;
            let scope = lifecycle_scope(&params.cleanup_scope_id, &params.remaining_budget_nanos);
            let dependency_context = context.dependency_invocation_context()?;
            let pending_key = lifecycle_pending_key(&scope.scope_id);
            let (event_sender, event_receiver) = mpsc::unbounded();
            if generation
                .pending
                .lock()
                .expect("Process V2 pending")
                .insert(pending_key.clone(), event_sender)
                .is_some()
            {
                return Err(protocol_failure(generation.execution_class));
            }
            let (sender, receiver) = futures::channel::oneshot::channel();
            generation
                .stopped_result
                .lock()
                .expect("stopped slot")
                .replace(sender);
            generation.send(&HostFrameV2::Stop(params.clone()))?;
            let result = await_lifecycle(
                receiver,
                event_receiver,
                dependency_context,
                context.cancellation(),
                &generation,
                &initialization,
                &scope,
            )
            .await;
            generation
                .pending
                .lock()
                .expect("Process V2 pending")
                .remove(&pending_key);
            let result = result?;
            result
                .validate_for(&params)
                .map_err(|error| protocol(generation.execution_class, error))?;
            generation.imports.deactivate();
            if let Some(mut child) = generation.child.lock().expect("Process V2 child").take() {
                child
                    .wait()
                    .map_err(|error| RuntimeFailure::PluginFailure {
                        detail: format!("failed to reap Process V2 Plugin: {error}"),
                    })?;
            }
            generation.stopped.store(true, Ordering::Release);
            if result.hook == StopHookOutcome::Failed {
                return Err(RuntimeFailure::PluginFailure {
                    detail: result.diagnostics.first().map_or_else(
                        || "Process V2 stop failed".to_owned(),
                        |value| value.detail.clone(),
                    ),
                });
            }
            Ok(())
        })
    }
}

async fn await_lifecycle<T>(
    receiver: futures::channel::oneshot::Receiver<Result<T, RuntimeFailure>>,
    mut events: mpsc::UnboundedReceiver<InvocationEvent>,
    dependency_context: InvocationContext,
    cancellation: CancellationToken,
    generation: &ProcessGenerationV2,
    initialization: &InitializeParams,
    scope: &InvocationScope,
) -> Result<T, RuntimeFailure> {
    let mut response = receiver.fuse();
    loop {
        let event = events.next().fuse();
        let cancelled = cancellation.cancelled().fuse();
        futures::pin_mut!(event, cancelled);
        select! {
            result = response => return result.map_err(|_| unavailable())?,
            event = event => match event.ok_or_else(unavailable)? {
                InvocationEvent::Outbound(call) => {
                    dispatch_outbound(
                        generation,
                        initialization,
                        scope,
                        call,
                        dependency_context.clone(),
                    ).await?;
                }
                InvocationEvent::Outcome(_)
                | InvocationEvent::CancelAck(_)
                | InvocationEvent::Settlement(_) => {
                    generation.abort();
                    return Err(protocol_failure(generation.execution_class));
                }
            },
            () = cancelled => {
                let _ = generation.send(&HostFrameV2::Cancel(CancelParams {
                    session: generation.identity.session.clone(),
                    scope_id: scope.scope_id.clone(),
                    correlation_id: "0".to_owned(),
                    reason: "Host lifecycle scope was cancelled".to_owned(),
                }));
                generation.abort();
                return Err(RuntimeFailure::AdmissionClosed);
            }
        }
    }
}

async fn await_initialization<T>(
    receiver: futures::channel::oneshot::Receiver<Result<T, RuntimeFailure>>,
    context: &ActivateContext,
    generation: &ProcessGenerationV2,
) -> Result<T, RuntimeFailure> {
    let response = receiver.fuse();
    let cancelled = context.cancellation().cancelled().fuse();
    futures::pin_mut!(response, cancelled);
    select! {
        response = response => response.map_err(|_| unavailable())?,
        () = cancelled => {
            generation.abort();
            Err(RuntimeFailure::AdmissionClosed)
        }
    }
}

impl Drop for ProcessGenerationV2 {
    fn drop(&mut self) {
        self.abort();
        if let Some(reader) = self.reader.lock().expect("Process V2 reader").take() {
            let _ = reader.join();
        }
    }
}

fn spawn_reader(
    stdout: impl io::Read + Send + 'static,
    initialized: LifecycleSlot<InitializeParams>,
    constructed: LifecycleSlot<lenso_process_protocol::authoring::ConstructedResult>,
    stopped: LifecycleSlot<lenso_process_protocol::authoring::StoppedResult>,
    pending: PendingInvocations,
    failed: Arc<AtomicBool>,
    limit: usize,
) -> Result<thread::JoinHandle<()>, RuntimeFailure> {
    thread::Builder::new()
        .name("lenso-process-v2-reader".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let result = read_frame::<GuestFrameV2>(&mut reader, limit);
                let frame = match result {
                    Ok(frame) => frame,
                    Err(detail) => {
                        failed.store(true, Ordering::Release);
                        retire_all(&initialized, &constructed, &stopped, &pending, &detail);
                        return;
                    }
                };
                let delivered = match frame {
                    GuestFrameV2::Initialized(value) => send_lifecycle(&initialized, value),
                    GuestFrameV2::Constructed(value) => send_lifecycle(&constructed, value),
                    GuestFrameV2::Stopped(value) => {
                        let delivered = send_lifecycle(&stopped, value);
                        if !delivered {
                            failed.store(true, Ordering::Release);
                            retire_all(
                                &initialized,
                                &constructed,
                                &stopped,
                                &pending,
                                "Process V2 returned an unexpected stop result",
                            );
                        }
                        return;
                    }
                    GuestFrameV2::InvocationResult(value) => send_invocation(
                        &pending,
                        &value.correlation_id.clone(),
                        InvocationEvent::Outcome(value),
                    ),
                    GuestFrameV2::OutboundCall(value) => send_invocation(
                        &pending,
                        &parent_pending_key(&value.scope),
                        InvocationEvent::Outbound(value),
                    ),
                    GuestFrameV2::CancelAck(value) => send_invocation(
                        &pending,
                        &value.correlation_id.clone(),
                        InvocationEvent::CancelAck(value),
                    ),
                    GuestFrameV2::Settlement(value) => send_invocation(
                        &pending,
                        &value.correlation_id.clone(),
                        InvocationEvent::Settlement(value),
                    ),
                };
                if !delivered {
                    failed.store(true, Ordering::Release);
                    retire_all(
                        &initialized,
                        &constructed,
                        &stopped,
                        &pending,
                        "Process V2 returned an unexpected frame",
                    );
                    return;
                }
            }
        })
        .map_err(internal)
}

fn send_lifecycle<T>(slot: &LifecycleSlot<T>, value: T) -> bool {
    slot.lock()
        .expect("Process V2 lifecycle slot")
        .take()
        .is_some_and(|sender| sender.send(Ok(value)).is_ok())
}

fn send_invocation(pending: &PendingInvocations, id: &str, event: InvocationEvent) -> bool {
    pending
        .lock()
        .expect("Process V2 pending")
        .get_mut(id)
        .is_some_and(|sender| sender.unbounded_send(event).is_ok())
}

fn retire_all(
    initialized: &LifecycleSlot<InitializeParams>,
    constructed: &LifecycleSlot<lenso_process_protocol::authoring::ConstructedResult>,
    stopped: &LifecycleSlot<lenso_process_protocol::authoring::StoppedResult>,
    pending: &PendingInvocations,
    detail: &str,
) {
    let error = || RuntimeFailure::PluginFailure {
        detail: detail.chars().take(512).collect(),
    };
    if let Some(sender) = initialized.lock().ok().and_then(|mut value| value.take()) {
        let _ = sender.send(Err(error()));
    }
    if let Some(sender) = constructed.lock().ok().and_then(|mut value| value.take()) {
        let _ = sender.send(Err(error()));
    }
    if let Some(sender) = stopped.lock().ok().and_then(|mut value| value.take()) {
        let _ = sender.send(Err(error()));
    }
    pending.lock().expect("Process V2 pending").clear();
}

fn read_frame<T: DeserializeOwned>(
    reader: &mut impl io::BufRead,
    limit: usize,
) -> Result<T, String> {
    let mut bytes = Vec::new();
    let read = io::Read::take(&mut *reader, limit as u64 + 1)
        .read_until(b'\n', &mut bytes)
        .map_err(|error| format!("failed to read Process V2 frame: {error}"))?;
    if read == 0 {
        return Err("Process V2 Plugin exited".to_owned());
    }
    if bytes.len() > limit || !bytes.ends_with(b"\n") {
        return Err("Process V2 frame exceeds the configured limit".to_owned());
    }
    bytes.pop();
    lenso_process_protocol::decode_strict(&bytes).map_err(|error| error.to_string())
}

fn invocation_scope(
    context: &InvocationContext,
    execution_class: &'static str,
) -> Result<InvocationScope, RuntimeFailure> {
    let mut extensions = context
        .extensions()
        .map(|extension| lenso_process_protocol::InvocationExtension {
            key: extension.key().to_owned(),
            value: STANDARD.encode(extension.value()),
            issuer: None,
            audience: Vec::new(),
            proof: None,
            sealed: false,
        })
        .chain(context.sealed_extensions().map(|extension| {
            lenso_process_protocol::InvocationExtension {
                key: extension.key().to_owned(),
                value: STANDARD.encode(extension.value()),
                issuer: Some(extension.issuer().to_owned()),
                audience: extension.audience().to_vec(),
                proof: Some(extension.proof().to_owned()),
                sealed: true,
            }
        }))
        .collect::<Vec<_>>();
    extensions.sort_by(|left, right| left.key.cmp(&right.key));
    let scope = InvocationScope {
        scope_id: format!("invoke-{}", context.request_id()),
        parent_scope_id: None,
        remaining_budget_nanos: duration_nanos(context.remaining_budget()),
        permissions: Vec::new(),
        extensions,
    };
    scope
        .validate()
        .map_err(|error| protocol(execution_class, error))?;
    Ok(scope)
}

fn duration_nanos(duration: Option<std::time::Duration>) -> String {
    duration.map_or_else(
        || u64::MAX.to_string(),
        |value| value.as_nanos().min(u128::from(u64::MAX)).to_string(),
    )
}

async fn dispatch_outbound(
    generation: &ProcessGenerationV2,
    initialization: &InitializeParams,
    parent: &InvocationScope,
    call: OutboundCallParams,
    context: InvocationContext,
) -> Result<(), RuntimeFailure> {
    if call.validate_against(initialization, parent, true).is_err() {
        generation.abort();
        return Err(protocol_failure(generation.execution_class));
    }
    let binding_id = match route_binding_id(initialization, &call, generation.execution_class) {
        Ok(binding_id) => binding_id,
        Err(error) => {
            generation.abort();
            return Err(error);
        }
    };
    let result = generation
        .imports
        .invoke(
            binding_id,
            call.operation.clone(),
            call.payload.clone(),
            context,
        )
        .await;
    generation.send(&HostFrameV2::OutboundResult(InvocationResult {
        session: call.session,
        correlation_id: call.correlation_id,
        outcome: wire_outcome(result),
    }))
}

fn route_binding_id(
    initialization: &InitializeParams,
    call: &OutboundCallParams,
    execution_class: &'static str,
) -> Result<u32, RuntimeFailure> {
    let route = initialization
        .routes
        .iter()
        .find(|route| route.route_id == call.route_id)
        .ok_or_else(|| protocol_failure(execution_class))?;
    route
        .route_id
        .strip_prefix("route-")
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| protocol_failure(execution_class))
}

fn parent_pending_key(scope: &InvocationScope) -> String {
    let parent = scope.parent_scope_id.as_deref().unwrap_or_default();
    parent
        .strip_prefix("invoke-")
        .map_or_else(|| lifecycle_pending_key(parent), str::to_owned)
}

fn lifecycle_pending_key(scope_id: &str) -> String {
    format!("lifecycle:{scope_id}")
}

fn lifecycle_scope(scope_id: &str, remaining_budget_nanos: &str) -> InvocationScope {
    InvocationScope {
        scope_id: scope_id.to_owned(),
        parent_scope_id: None,
        remaining_budget_nanos: remaining_budget_nanos.to_owned(),
        permissions: Vec::new(),
        extensions: Vec::new(),
    }
}

fn cardinality(value: CapabilityCardinality) -> RequirementCardinality {
    match value {
        CapabilityCardinality::One => RequirementCardinality::One,
        CapabilityCardinality::Optional => RequirementCardinality::Optional,
        CapabilityCardinality::Many => RequirementCardinality::Many,
    }
}

fn contract_digest(
    instance: &PluginInstancePlan,
    provided: &[Rc<dyn JsonCapabilityCodec>],
    required: &[Rc<dyn JsonCapabilityCodec>],
) -> String {
    let provided_codecs = provided
        .iter()
        .map(|codec| (codec.capability_id(), codec.as_ref()))
        .collect::<BTreeMap<_, _>>();
    let required_codecs = required
        .iter()
        .map(|codec| (codec.capability_id(), codec.as_ref()))
        .collect::<BTreeMap<_, _>>();
    let mut identities = instance
        .provided_capabilities()
        .iter()
        .map(|endpoint| {
            let codec = provided_codecs[endpoint.capability_id()];
            let mut operations = endpoint.request_operations();
            operations.sort_unstable();
            format!(
                "provide:{}:{}:{}:{}",
                endpoint.capability_id(),
                endpoint.descriptor_version(),
                codec.descriptor_digest(),
                operations.join(",")
            )
        })
        .chain(instance.required_capabilities().iter().map(|requirement| {
            let codec = required_codecs[requirement.capability_id()];
            format!(
                "require:{}:{}:{}:{}:{:?}",
                requirement.requirement_id(),
                requirement.capability_id(),
                requirement.descriptor_version(),
                codec.descriptor_digest(),
                requirement.cardinality()
            )
        }))
        .collect::<Vec<_>>();
    identities.sort();
    format!(
        "sha256:{}",
        hex::encode(Sha256::digest(identities.join("\n").as_bytes()))
    )
}

fn validate_digest(value: &str, capability: &'static str) -> Result<(), RuntimeFailure> {
    let valid = value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    if valid {
        Ok(())
    } else {
        Err(RuntimeFailure::ProtocolViolation { capability })
    }
}

fn wire_outcome(result: Result<JsonInvocationOutcome, RuntimeFailure>) -> InvocationOutcome {
    match result {
        Ok(JsonInvocationOutcome::Success(value)) => InvocationOutcome::Success { value },
        Ok(JsonInvocationOutcome::DomainError(error)) => InvocationOutcome::Domain { error },
        Err(failure) => InvocationOutcome::Runtime {
            failure: wire_failure(failure),
        },
    }
}

fn from_wire_outcome(
    value: InvocationOutcome,
    execution_class: &'static str,
) -> Result<JsonInvocationOutcome, RuntimeFailure> {
    match value {
        InvocationOutcome::Success { value } => Ok(JsonInvocationOutcome::Success(value)),
        InvocationOutcome::Domain { error } => Ok(JsonInvocationOutcome::DomainError(error)),
        InvocationOutcome::Runtime { failure } => Err(from_wire_failure(failure, execution_class)),
    }
}

fn wire_failure(value: RuntimeFailure) -> WireFailure {
    match value {
        RuntimeFailure::Unavailable { capability } => WireFailure::Unavailable {
            capability: capability.to_owned(),
        },
        RuntimeFailure::UnknownOperation {
            capability,
            operation,
        } => WireFailure::UnknownOperation {
            capability: capability.to_owned(),
            operation,
        },
        RuntimeFailure::AmbiguousBinding {
            capability,
            providers,
        } => WireFailure::AmbiguousBinding {
            capability: capability.to_owned(),
            providers: u32::try_from(providers).unwrap_or(u32::MAX),
        },
        RuntimeFailure::ProtocolViolation { capability } => WireFailure::ProtocolViolation {
            capability: capability.to_owned(),
        },
        RuntimeFailure::AdmissionClosed => WireFailure::AdmissionClosed,
        RuntimeFailure::ResourceExhausted {
            capability,
            operation,
        } => WireFailure::ResourceExhausted {
            capability: capability.to_owned(),
            operation,
        },
        RuntimeFailure::DeadlineExceeded { request_id } => WireFailure::DeadlineExceeded {
            request_id: request_id.to_string(),
        },
        RuntimeFailure::Cancelled { request_id } => WireFailure::Cancelled {
            request_id: request_id.to_string(),
        },
        other => WireFailure::Internal {
            detail: format!("{other:?}"),
        },
    }
}

fn from_wire_failure(value: WireFailure, execution_class: &'static str) -> RuntimeFailure {
    match value {
        WireFailure::Unavailable { capability } => RuntimeFailure::PluginFailure {
            detail: format!("unavailable Capability `{capability}`"),
        },
        WireFailure::UnknownOperation {
            capability: _,
            operation,
        } => RuntimeFailure::UnknownOperation {
            capability: execution_class,
            operation,
        },
        WireFailure::AmbiguousBinding {
            capability: _,
            providers,
        } => RuntimeFailure::AmbiguousBinding {
            capability: execution_class,
            providers: providers as usize,
        },
        WireFailure::ProtocolViolation { .. } => protocol_failure(execution_class),
        WireFailure::AdmissionClosed => RuntimeFailure::AdmissionClosed,
        WireFailure::ResourceExhausted { operation, .. } => RuntimeFailure::ResourceExhausted {
            capability: execution_class,
            operation,
        },
        WireFailure::DeadlineExceeded { request_id } => RuntimeFailure::DeadlineExceeded {
            request_id: request_id.parse().unwrap_or_default(),
        },
        WireFailure::Cancelled { request_id } => RuntimeFailure::Cancelled {
            request_id: request_id.parse().unwrap_or_default(),
        },
        WireFailure::PluginFailure { detail } => RuntimeFailure::PluginFailure { detail },
        other => RuntimeFailure::PluginFailure {
            detail: format!("{other:?}"),
        },
    }
}

fn arm_termination(
    child: Arc<Mutex<Option<Child>>>,
    pending: PendingInvocations,
    failed: Arc<AtomicBool>,
    correlation_id: String,
    timeout: std::time::Duration,
) {
    let _ = thread::Builder::new()
        .name(format!("lenso-process-v2-cancel-{correlation_id}"))
        .spawn(move || {
            thread::sleep(timeout);
            if pending
                .lock()
                .expect("Process V2 pending")
                .contains_key(&correlation_id)
            {
                failed.store(true, Ordering::Release);
                if let Some(mut child) = child.lock().expect("Process V2 child").take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        });
}

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn protocol(execution_class: &'static str, error: impl std::fmt::Display) -> RuntimeFailure {
    let _ = error;
    protocol_failure(execution_class)
}

fn protocol_failure(execution_class: &'static str) -> RuntimeFailure {
    RuntimeFailure::ProtocolViolation {
        capability: execution_class,
    }
}

fn unavailable() -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: "Process V2 generation is unavailable".to_owned(),
    }
}

fn internal(error: impl std::fmt::Display) -> RuntimeFailure {
    RuntimeFailure::Internal {
        detail: error.to_string(),
    }
}
