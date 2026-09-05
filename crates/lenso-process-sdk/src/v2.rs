use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, BufRead as _, BufReader, BufWriter, Write as _},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
};

use lenso_process_protocol::authoring::{
    CancelAck, CancelParams, ConstructParams, ConstructedResult, FactoryOutcome, InitializeParams,
    InitializedResult, InvocationOutcome, InvocationResult, InvocationScope, InvokeParams,
    OutboundCallParams, OutboundCallResult, SessionIdentity, Settlement, SettlementState,
    StopHookOutcome, StopParams, StoppedResult,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Multiplexed stdio transport selected by the Process Authoring V2 profile.
pub const PROTOCOL_VERSION_V2: &str = "lenso.process-stdio@2";

/// Host-to-guest Process V2 envelope.
#[derive(Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum HostFrameV2 {
    Initialize(InitializeParams),
    Construct(ConstructParams),
    Invoke(InvokeParams),
    OutboundResult(OutboundCallResult),
    Cancel(CancelParams),
    Stop(StopParams),
}

/// Guest-to-Host Process V2 envelope.
#[derive(Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum GuestFrameV2 {
    Initialized(InitializedResult),
    Constructed(ConstructedResult),
    InvocationResult(InvocationResult),
    OutboundCall(OutboundCallParams),
    CancelAck(CancelAck),
    Settlement(Settlement),
    Stopped(StoppedResult),
}

/// Result of running the optional Plugin cleanup hook.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProcessStopOutcome {
    NotDeclared,
    Completed,
    Failed(String),
}

/// Complete Process Plugin implementation hosted by the V2 SDK loop.
pub trait ProcessPluginV2: Send + Sync + 'static {
    type Instance: Send + Sync + 'static;

    /// Performs runtime-local admission before the Host may construct the object.
    fn initialize(&self, _params: &InitializeParams) -> Result<(), String> {
        Ok(())
    }

    /// Constructs exactly one complete object for this admitted generation.
    fn construct(
        &self,
        params: &ConstructParams,
        context: ProcessLifecycleContext,
    ) -> Result<Self::Instance, String>;

    /// Invokes one declared endpoint on the complete object.
    fn invoke(
        &self,
        instance: &Self::Instance,
        params: InvokeParams,
        context: ProcessInvocationContext,
    ) -> InvocationOutcome;

    /// Stops the complete object once. The default represents no declared hook.
    fn stop(
        &self,
        _instance: &Self::Instance,
        _params: &StopParams,
        _context: ProcessLifecycleContext,
    ) -> ProcessStopOutcome {
        ProcessStopOutcome::NotDeclared
    }
}

type SharedWriter = Arc<Mutex<Box<dyn io::Write + Send>>>;
type OutboundWaiters = Arc<Mutex<BTreeMap<String, mpsc::SyncSender<OutboundCallResult>>>>;

/// Scope-bound authority for cancellation observation and exact outbound routes.
#[derive(Clone)]
pub struct ProcessCallContext {
    identity: SessionIdentity,
    parent: InvocationScope,
    cancelled: Arc<AtomicBool>,
    writer: SharedWriter,
    outbound: OutboundWaiters,
    next_outbound_id: Arc<AtomicU64>,
    max_frame_bytes: usize,
    max_active_outbound_calls: usize,
}

impl std::fmt::Debug for ProcessCallContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessCallContext")
            .field("session", &self.identity.session)
            .field("parent_scope", &self.parent.scope_id)
            .field("cancelled", &self.cancelled)
            .finish_non_exhaustive()
    }
}

impl ProcessCallContext {
    /// Returns whether the Host requested cooperative cancellation.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Returns the Host-provided remaining budget as canonical nanoseconds.
    #[must_use]
    pub fn remaining_budget_nanos(&self) -> &str {
        &self.parent.remaining_budget_nanos
    }

    /// Calls one exact Host-provided route while preserving invocation authority.
    pub fn call(
        &self,
        requirement_id: impl Into<String>,
        route_id: impl Into<String>,
        operation: impl Into<String>,
        payload: Value,
    ) -> Result<InvocationOutcome, String> {
        if self.is_cancelled() {
            return Err("parent invocation was cancelled".to_owned());
        }
        let id = self.next_outbound_id.fetch_add(1, Ordering::Relaxed);
        if id == 0 {
            return Err("outbound correlation identity exhausted".to_owned());
        }
        let correlation_id = id.to_string();
        let request = OutboundCallParams {
            session: self.identity.session.clone(),
            correlation_id: correlation_id.clone(),
            requirement_id: requirement_id.into(),
            route_id: route_id.into(),
            operation: operation.into(),
            scope: InvocationScope {
                scope_id: format!("{}:outbound:{correlation_id}", self.parent.scope_id),
                parent_scope_id: Some(self.parent.scope_id.clone()),
                remaining_budget_nanos: self.parent.remaining_budget_nanos.clone(),
                permissions: self.parent.permissions.clone(),
                extensions: self.parent.extensions.clone(),
            },
            payload,
        };
        request
            .validate_for(&self.identity, &self.parent)
            .map_err(|error| error.to_string())?;
        let (sender, receiver) = mpsc::sync_channel(1);
        {
            let mut outbound = self.outbound.lock().expect("Process outbound waiters");
            if outbound.len() >= self.max_active_outbound_calls {
                return Err("active outbound call limit exceeded".to_owned());
            }
            outbound.insert(correlation_id.clone(), sender);
        }
        if let Err(error) = write_v2_frame(
            &self.writer,
            &GuestFrameV2::OutboundCall(request.clone()),
            self.max_frame_bytes,
        ) {
            self.outbound
                .lock()
                .expect("Process outbound waiters")
                .remove(&correlation_id);
            return Err(error.to_string());
        }
        let result = receiver
            .recv()
            .map_err(|_| "Host closed the outbound result channel".to_owned())?;
        result
            .validate_for_outbound(&request)
            .map_err(|error| error.to_string())?;
        Ok(result.outcome)
    }
}

/// Call authority supplied while a Plugin operation is running.
pub type ProcessInvocationContext = ProcessCallContext;

/// Call authority supplied while a Plugin is being constructed or stopped.
pub type ProcessLifecycleContext = ProcessCallContext;

#[derive(Debug)]
struct ActiveInvocation {
    scope_id: String,
    cancelled: Arc<AtomicBool>,
}

#[derive(Debug)]
struct ActiveLifecycle {
    scope_id: String,
    correlation_id: &'static str,
    cancelled: Arc<AtomicBool>,
}

enum LoopEvent<I> {
    Host(Box<HostFrameV2>),
    HostClosed,
    HostFailed(io::Error),
    Constructed {
        params: ConstructParams,
        result: Result<I, String>,
    },
    Stopped {
        params: StopParams,
        outcome: ProcessStopOutcome,
    },
}

/// Serves one complete-object Process Plugin over multiplexed V2 stdio.
pub fn serve_v2(plugin: impl ProcessPluginV2) -> io::Result<()> {
    serve_v2_with_profile_and_limit(plugin, PROTOCOL_VERSION_V2, super::DEFAULT_MAX_FRAME_BYTES)
}

/// Serves one Process V2 Plugin with an explicit encoded frame bound.
pub fn serve_v2_with_limit(plugin: impl ProcessPluginV2, max_frame_bytes: usize) -> io::Result<()> {
    serve_v2_with_profile_and_limit(plugin, PROTOCOL_VERSION_V2, max_frame_bytes)
}

/// Serves one complete-object Plugin for a language Adapter's exact V2 profile.
pub fn serve_v2_with_profile(
    plugin: impl ProcessPluginV2,
    runtime_profile: &'static str,
) -> io::Result<()> {
    serve_v2_with_profile_and_limit(plugin, runtime_profile, super::DEFAULT_MAX_FRAME_BYTES)
}

/// Serves a language Adapter profile with an explicit encoded frame bound.
pub fn serve_v2_with_profile_and_limit(
    plugin: impl ProcessPluginV2,
    runtime_profile: &'static str,
    max_frame_bytes: usize,
) -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    serve_v2_io(
        &Arc::new(plugin),
        BufReader::new(stdin),
        Box::new(BufWriter::new(stdout)),
        runtime_profile,
        max_frame_bytes,
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "the exhaustive wire-state dispatch is kept together so sequencing constraints remain visible"
)]
fn serve_v2_io<P: ProcessPluginV2>(
    plugin: &Arc<P>,
    mut reader: impl io::BufRead + Send + 'static,
    writer: Box<dyn io::Write + Send>,
    runtime_profile: &'static str,
    max_frame_bytes: usize,
) -> io::Result<()> {
    let writer = Arc::new(Mutex::new(writer));
    let (loop_sender, loop_receiver) = mpsc::channel::<LoopEvent<P::Instance>>();
    let reader_sender = loop_sender.clone();
    thread::Builder::new()
        .name("lenso-process-control".to_owned())
        .spawn(move || {
            loop {
                let event = match read_v2_frame(&mut reader, max_frame_bytes) {
                    Ok(Some(frame)) => LoopEvent::Host(Box::new(frame)),
                    Ok(None) => LoopEvent::HostClosed,
                    Err(error) => LoopEvent::HostFailed(error),
                };
                let terminal = matches!(event, LoopEvent::HostClosed | LoopEvent::HostFailed(_));
                if reader_sender.send(event).is_err() || terminal {
                    break;
                }
            }
        })?;

    let initialize = match receive_host_event(&loop_receiver)? {
        Some(HostFrameV2::Initialize(params)) => params,
        Some(_) => return Err(invalid_data("Process V2 requires initialize first")),
        None => return Ok(()),
    };
    initialize
        .validate_for_runtime_profile(runtime_profile)
        .map_err(protocol_error)?;
    plugin.initialize(&initialize).map_err(invalid_data)?;
    write_v2_frame(
        &writer,
        &GuestFrameV2::Initialized(initialize.clone()),
        max_frame_bytes,
    )?;

    let mut instance: Option<Arc<P::Instance>> = None;
    let mut construction_attempted = false;
    let active = Arc::new(Mutex::new(BTreeMap::<String, ActiveInvocation>::new()));
    let retired = Arc::new(Mutex::new(BTreeSet::<String>::new()));
    let outbound = Arc::new(Mutex::new(BTreeMap::new()));
    let next_outbound_id = Arc::new(AtomicU64::new(1));
    let mut lifecycle: Option<ActiveLifecycle> = None;

    loop {
        match loop_receiver
            .recv()
            .map_err(|_| invalid_data("Process V2 control loop stopped"))?
        {
            LoopEvent::Host(frame) => match *frame {
                HostFrameV2::Initialize(_) => {
                    return Err(invalid_data("Process V2 initialized more than once"));
                }
                HostFrameV2::Construct(params) => {
                    params
                        .validate_for(&initialize.identity)
                        .map_err(protocol_error)?;
                    if construction_attempted || lifecycle.is_some() {
                        return Err(invalid_data("Process V2 constructed more than once"));
                    }
                    construction_attempted = true;
                    let cancelled = Arc::new(AtomicBool::new(false));
                    lifecycle = Some(ActiveLifecycle {
                        scope_id: params.lifecycle_scope_id.clone(),
                        correlation_id: "0",
                        cancelled: cancelled.clone(),
                    });
                    let context = process_call_context(
                        &initialize,
                        lifecycle_scope(&params.lifecycle_scope_id, &params.remaining_budget_nanos),
                        cancelled,
                        writer.clone(),
                        outbound.clone(),
                        next_outbound_id.clone(),
                        max_frame_bytes,
                    );
                    let plugin = plugin.clone();
                    let sender = loop_sender.clone();
                    thread::Builder::new()
                        .name("lenso-process-construct".to_owned())
                        .spawn(move || {
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    plugin.construct(&params, context)
                                }))
                                .unwrap_or_else(|_| {
                                    Err("Process Plugin construction panicked".to_owned())
                                });
                            let _ = sender.send(LoopEvent::Constructed { params, result });
                        })?;
                }
                HostFrameV2::Invoke(params) => {
                    params
                        .validate_against(&initialize)
                        .map_err(protocol_error)?;
                    if lifecycle.is_some() {
                        return Err(invalid_data(
                            "Process V2 invoked while lifecycle work was active",
                        ));
                    }
                    if retired.lock().expect("Process retired invocations").len()
                        >= initialize.limits.max_retired_ids as usize
                    {
                        return Err(invalid_data("Process V2 retired identity limit exhausted"));
                    }
                    let object = instance
                        .clone()
                        .ok_or_else(|| invalid_data("Process V2 invoked before construction"))?;
                    let correlation_id = params.correlation_id.clone();
                    let scope_id = params.scope.scope_id.clone();
                    let cancelled = Arc::new(AtomicBool::new(false));
                    {
                        let mut active = active.lock().expect("Process active invocations");
                        if retired
                            .lock()
                            .expect("Process retired invocations")
                            .contains(&correlation_id)
                        {
                            return Err(invalid_data("Process V2 reused a retired correlation id"));
                        }
                        if active.len() >= initialize.limits.max_active_invocations as usize {
                            write_v2_frame(
                            &writer,
                            &GuestFrameV2::InvocationResult(InvocationResult {
                                session: params.session.clone(),
                                correlation_id: correlation_id.clone(),
                                outcome: InvocationOutcome::Runtime {
                                    failure: lenso_process_protocol::authoring::RuntimeFailure::ResourceExhausted {
                                        capability: params.capability_id,
                                        operation: params.operation,
                                    },
                                },
                            }),
                            max_frame_bytes,
                        )?;
                            write_v2_frame(
                                &writer,
                                &GuestFrameV2::Settlement(Settlement {
                                    session: params.session,
                                    scope_id,
                                    correlation_id,
                                    state: SettlementState::Completed,
                                }),
                                max_frame_bytes,
                            )?;
                            continue;
                        }
                        if active
                            .insert(
                                correlation_id.clone(),
                                ActiveInvocation {
                                    scope_id: scope_id.clone(),
                                    cancelled: cancelled.clone(),
                                },
                            )
                            .is_some()
                        {
                            return Err(invalid_data("Process V2 reused an active correlation id"));
                        }
                    }
                    let plugin = plugin.clone();
                    let writer = writer.clone();
                    let active_for_worker = active.clone();
                    let retired_for_worker = retired.clone();
                    let outbound = outbound.clone();
                    let next_outbound_id = next_outbound_id.clone();
                    let identity = initialize.identity.clone();
                    let max_active_outbound_calls =
                        initialize.limits.max_active_outbound_calls as usize;
                    let worker_correlation_id = correlation_id.clone();
                    let spawn = thread::Builder::new()
                    .name(format!("lenso-process-invoke-{correlation_id}"))
                    .spawn(move || {
                        let context = ProcessCallContext {
                            identity: identity.clone(),
                            parent: params.scope.clone(),
                            cancelled: cancelled.clone(),
                            writer: writer.clone(),
                            outbound,
                            next_outbound_id,
                            max_frame_bytes,
                            max_active_outbound_calls,
                        };
                        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            plugin.invoke(&object, params, context)
                        }));
                        let (outcome, panicked) = match outcome {
                            Ok(outcome) => (outcome, false),
                            Err(_) => (
                                InvocationOutcome::Runtime {
                                    failure: lenso_process_protocol::authoring::RuntimeFailure::Internal {
                                        detail: "Process Plugin invocation panicked".to_owned(),
                                    },
                                },
                                true,
                            ),
                        };
                        let _ = write_v2_frame(
                            &writer,
                            &GuestFrameV2::InvocationResult(InvocationResult {
                                session: identity.session.clone(),
                                correlation_id: worker_correlation_id.clone(),
                                outcome,
                            }),
                            max_frame_bytes,
                        );
                        active_for_worker
                            .lock()
                            .expect("Process active invocations")
                            .remove(&worker_correlation_id);
                        retired_for_worker
                            .lock()
                            .expect("Process retired invocations")
                            .insert(worker_correlation_id.clone());
                        let state = if panicked {
                            SettlementState::Abandoned
                        } else if cancelled.load(Ordering::Acquire) {
                            SettlementState::Cancelled
                        } else {
                            SettlementState::Completed
                        };
                        let _ = write_v2_frame(
                            &writer,
                            &GuestFrameV2::Settlement(Settlement {
                                session: identity.session,
                                scope_id,
                                correlation_id: worker_correlation_id,
                                state,
                            }),
                            max_frame_bytes,
                        );
                    });
                    if let Err(error) = spawn {
                        active
                            .lock()
                            .expect("Process active invocations")
                            .remove(&correlation_id);
                        return Err(error);
                    }
                }
                HostFrameV2::OutboundResult(result) => {
                    let sender = outbound
                        .lock()
                        .expect("Process outbound waiters")
                        .remove(&result.correlation_id)
                        .ok_or_else(|| invalid_data("unknown outbound correlation id"))?;
                    sender
                        .send(result)
                        .map_err(|_| invalid_data("outbound caller stopped before its result"))?;
                }
                HostFrameV2::Cancel(params) => {
                    params
                        .validate_for(&initialize.identity)
                        .map_err(protocol_error)?;
                    let accepted = active
                        .lock()
                        .expect("Process active invocations")
                        .get(&params.correlation_id)
                        .is_some_and(|invocation| {
                            if invocation.scope_id != params.scope_id {
                                return false;
                            }
                            invocation.cancelled.store(true, Ordering::Release);
                            true
                        })
                        || lifecycle.as_ref().is_some_and(|execution| {
                            if execution.scope_id != params.scope_id
                                || execution.correlation_id != params.correlation_id
                            {
                                return false;
                            }
                            execution.cancelled.store(true, Ordering::Release);
                            true
                        });
                    write_v2_frame(
                        &writer,
                        &GuestFrameV2::CancelAck(CancelAck {
                            session: params.session,
                            scope_id: params.scope_id,
                            correlation_id: params.correlation_id,
                            accepted,
                        }),
                        max_frame_bytes,
                    )?;
                }
                HostFrameV2::Stop(params) => {
                    params
                        .validate_for(&initialize.identity)
                        .map_err(protocol_error)?;
                    if lifecycle.is_some() {
                        return Err(invalid_data(
                            "Process V2 stopped while lifecycle work was active",
                        ));
                    }
                    if !active
                        .lock()
                        .expect("Process active invocations")
                        .is_empty()
                    {
                        return Err(invalid_data(
                            "Process V2 stopped with unfinished invocations",
                        ));
                    }
                    let object = instance
                        .as_ref()
                        .ok_or_else(|| invalid_data("Process V2 stopped before construction"))?;
                    let cancelled = Arc::new(AtomicBool::new(false));
                    lifecycle = Some(ActiveLifecycle {
                        scope_id: params.cleanup_scope_id.clone(),
                        correlation_id: "0",
                        cancelled: cancelled.clone(),
                    });
                    let context = process_call_context(
                        &initialize,
                        lifecycle_scope(&params.cleanup_scope_id, &params.remaining_budget_nanos),
                        cancelled,
                        writer.clone(),
                        outbound.clone(),
                        next_outbound_id.clone(),
                        max_frame_bytes,
                    );
                    let plugin = plugin.clone();
                    let object = object.clone();
                    let sender = loop_sender.clone();
                    thread::Builder::new()
                        .name("lenso-process-stop".to_owned())
                        .spawn(move || {
                            let outcome =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    plugin.stop(&object, &params, context)
                                }))
                                .unwrap_or_else(|_| {
                                    ProcessStopOutcome::Failed(
                                        "Process Plugin stop panicked".to_owned(),
                                    )
                                });
                            let _ = sender.send(LoopEvent::Stopped { params, outcome });
                        })?;
                }
            },
            LoopEvent::HostClosed => return Ok(()),
            LoopEvent::HostFailed(error) => return Err(error),
            LoopEvent::Constructed { params, result } => {
                let execution = lifecycle
                    .take()
                    .ok_or_else(|| invalid_data("unexpected Process V2 construction result"))?;
                if execution.scope_id != params.lifecycle_scope_id {
                    return Err(invalid_data("Process V2 construction scope mismatch"));
                }
                let outcome = match result {
                    Ok(value) if !execution.cancelled.load(Ordering::Acquire) => {
                        instance = Some(Arc::new(value));
                        FactoryOutcome::Constructed
                    }
                    Ok(_) => FactoryOutcome::Failed {
                        detail: "Process Plugin construction was cancelled".to_owned(),
                    },
                    Err(detail) => FactoryOutcome::Failed {
                        detail: bounded_detail(detail),
                    },
                };
                write_v2_frame(
                    &writer,
                    &GuestFrameV2::Constructed(ConstructedResult {
                        session: params.session,
                        lifecycle_scope_id: params.lifecycle_scope_id,
                        outcome,
                    }),
                    max_frame_bytes,
                )?;
            }
            LoopEvent::Stopped { params, outcome } => {
                let execution = lifecycle
                    .take()
                    .ok_or_else(|| invalid_data("unexpected Process V2 stop result"))?;
                if execution.scope_id != params.cleanup_scope_id {
                    return Err(invalid_data("Process V2 stop scope mismatch"));
                }
                let (hook, diagnostics) = match outcome {
                    ProcessStopOutcome::NotDeclared => (StopHookOutcome::NotDeclared, Vec::new()),
                    ProcessStopOutcome::Completed => (StopHookOutcome::Completed, Vec::new()),
                    ProcessStopOutcome::Failed(detail) => (
                        StopHookOutcome::Failed,
                        vec![lenso_process_protocol::authoring::CleanupDiagnostic {
                            code: "plugin_stop_failed".to_owned(),
                            detail: bounded_detail(detail),
                        }],
                    ),
                };
                write_v2_frame(
                    &writer,
                    &GuestFrameV2::Stopped(StoppedResult {
                        session: params.session,
                        cleanup_scope_id: params.cleanup_scope_id,
                        hook,
                        diagnostics,
                    }),
                    max_frame_bytes,
                )?;
                return Ok(());
            }
        }
    }
}

fn receive_host_event<I>(
    receiver: &mpsc::Receiver<LoopEvent<I>>,
) -> io::Result<Option<HostFrameV2>> {
    match receiver
        .recv()
        .map_err(|_| invalid_data("Process V2 control loop stopped"))?
    {
        LoopEvent::Host(frame) => Ok(Some(*frame)),
        LoopEvent::HostClosed => Ok(None),
        LoopEvent::HostFailed(error) => Err(error),
        LoopEvent::Constructed { .. } | LoopEvent::Stopped { .. } => Err(invalid_data(
            "Process V2 lifecycle ran before initialization",
        )),
    }
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

fn process_call_context(
    initialize: &InitializeParams,
    parent: InvocationScope,
    cancelled: Arc<AtomicBool>,
    writer: SharedWriter,
    outbound: OutboundWaiters,
    next_outbound_id: Arc<AtomicU64>,
    max_frame_bytes: usize,
) -> ProcessCallContext {
    ProcessCallContext {
        identity: initialize.identity.clone(),
        parent,
        cancelled,
        writer,
        outbound,
        next_outbound_id,
        max_frame_bytes,
        max_active_outbound_calls: initialize.limits.max_active_outbound_calls as usize,
    }
}

fn write_v2_frame(
    writer: &SharedWriter,
    frame: &GuestFrameV2,
    max_frame_bytes: usize,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(frame).map_err(protocol_error)?;
    if bytes.len() > max_frame_bytes {
        return Err(invalid_data(
            "Process V2 frame exceeds the configured limit",
        ));
    }
    let mut writer = writer.lock().expect("Process V2 writer");
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn read_v2_frame(
    reader: &mut impl io::BufRead,
    max_frame_bytes: usize,
) -> io::Result<Option<HostFrameV2>> {
    let mut bytes = Vec::new();
    let read = io::Read::take(
        &mut *reader,
        u64::try_from(max_frame_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1),
    )
    .read_until(b'\n', &mut bytes)?;
    if read == 0 {
        return Ok(None);
    }
    if bytes.len() > max_frame_bytes || !bytes.ends_with(b"\n") {
        return Err(invalid_data(
            "Process V2 frame exceeds the configured limit",
        ));
    }
    bytes.pop();
    lenso_process_protocol::decode_strict(&bytes)
        .map(Some)
        .map_err(protocol_error)
}

fn bounded_detail(mut detail: String) -> String {
    if detail.is_empty() {
        return "Plugin returned an empty failure detail".to_owned();
    }
    if detail.len() > 1_024 {
        let mut boundary = 1_024;
        while !detail.is_char_boundary(boundary) {
            boundary -= 1;
        }
        detail.truncate(boundary);
    }
    detail
}

fn invalid_data(detail: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, detail.into())
}

fn protocol_error(error: impl std::fmt::Display) -> io::Error {
    invalid_data(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct CountingWriter(Arc<AtomicU64>);

    impl io::Write for CountingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.fetch_add(bytes.len() as u64, Ordering::Relaxed);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn cancelled_parent_rejects_an_outbound_call_before_writing_a_frame() {
        let writes = Arc::new(AtomicU64::new(0));
        let context = ProcessInvocationContext {
            identity: SessionIdentity {
                session: "session-1".to_owned(),
                plugin_instance: "plugin".to_owned(),
                plugin_generation: "1".to_owned(),
                artifact_digest: format!("sha256:{}", "11".repeat(32)),
                contract_digest: format!("sha256:{}", "22".repeat(32)),
                runtime_profile: PROTOCOL_VERSION_V2.to_owned(),
                value_profile: lenso_process_protocol::VALUE_PROFILE.to_owned(),
            },
            parent: InvocationScope {
                scope_id: "invoke-1".to_owned(),
                parent_scope_id: None,
                remaining_budget_nanos: "1000".to_owned(),
                permissions: Vec::new(),
                extensions: Vec::new(),
            },
            cancelled: Arc::new(AtomicBool::new(true)),
            writer: Arc::new(Mutex::new(Box::new(CountingWriter(writes.clone())))),
            outbound: Arc::new(Mutex::new(BTreeMap::new())),
            next_outbound_id: Arc::new(AtomicU64::new(1)),
            max_frame_bytes: 4096,
            max_active_outbound_calls: 1,
        };

        let error = context
            .call("store", "route-1", "read", Value::Null)
            .unwrap_err();

        assert_eq!(error, "parent invocation was cancelled");
        assert_eq!(writes.load(Ordering::Relaxed), 0);
    }
}
