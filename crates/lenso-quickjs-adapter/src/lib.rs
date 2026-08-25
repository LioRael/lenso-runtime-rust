//! Bounded QuickJS-NG Execution Adapter for immutable bundled ES modules.

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use futures::{FutureExt, select};
use lenso_app_plan::{ExecutionClassId, ModuleInstancePlan, ResolvedAppPlan};
use lenso_kernel::{
    ExecutionAdapter, InvocationContext, ModuleLifecycle, PreparedNativeApp, PreparedNativeModule,
    RuntimeFailure,
};
use lenso_runtime_codec::{
    ArtifactCatalog, JsonCapabilityCodec, JsonInvocationOutcome, JsonRequestTransport,
    JsonStreamFrame, JsonStreamItem, JsonStreamOpenFuture, JsonStreamSessionTransport,
    JsonStreamTransport, codecs_for_instance, json_request_endpoints, json_stream_endpoints,
    prepare_request_app, validate_json_module_descriptor,
};
use rquickjs::{Context, Function, Module, Persistent, Runtime, promise::MaybePromise};
use serde_json::Value;

/// Stable open execution-class identity.
pub const EXECUTION_CLASS: &str = "lenso.quickjs@1";

/// Bounded `QuickJS` generation limits supplied by host policy.
#[derive(Clone, Debug)]
pub struct QuickJsLimits {
    pub max_module_bytes: usize,
    pub max_heap_bytes: usize,
    pub max_stack_bytes: usize,
    pub max_result_bytes: usize,
    pub max_pending_jobs: usize,
    pub max_streams: usize,
    pub max_turn: Duration,
}

impl Default for QuickJsLimits {
    fn default() -> Self {
        Self {
            max_module_bytes: 4 * 1024 * 1024,
            max_heap_bytes: 32 * 1024 * 1024,
            max_stack_bytes: 512 * 1024,
            max_result_bytes: 1024 * 1024,
            max_pending_jobs: 1024,
            max_streams: 1024,
            max_turn: Duration::from_secs(1),
        }
    }
}

/// QuickJS-NG Adapter configured with exact Artifact handles and generated codecs.
#[derive(Debug)]
pub struct QuickJsAdapter {
    artifacts: ArtifactCatalog,
    codecs: BTreeMap<String, Rc<dyn JsonCapabilityCodec>>,
    duplicate_codecs: BTreeSet<String>,
    limits: QuickJsLimits,
}

impl QuickJsAdapter {
    /// Creates one Adapter for a resolved Generation Artifact catalog.
    pub fn new(artifacts: ArtifactCatalog) -> Self {
        Self {
            artifacts,
            codecs: BTreeMap::new(),
            duplicate_codecs: BTreeSet::new(),
            limits: QuickJsLimits::default(),
        }
    }

    /// Registers the generated codec for one exact Capability Descriptor.
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

    /// Registers an already shared generated Capability codec.
    #[must_use]
    pub fn with_shared_codec(mut self, codec: Rc<dyn JsonCapabilityCodec>) -> Self {
        let capability = codec.capability_id().to_owned();
        if self.codecs.insert(capability.clone(), codec).is_some() {
            self.duplicate_codecs.insert(capability);
        }
        self
    }

    /// Applies host-policy resource limits.
    #[must_use]
    pub fn with_limits(mut self, limits: QuickJsLimits) -> Self {
        self.limits = limits;
        self
    }

    fn prepare_instance(
        &self,
        instance: &ModuleInstancePlan,
    ) -> Result<PreparedNativeModule, RuntimeFailure> {
        if instance.entrypoint().is_empty() || instance.entrypoint() == "default" {
            return invalid(format!(
                "QuickJS Instance `{}` needs an ES module entrypoint",
                instance.instance_key()
            ));
        }
        if !self.duplicate_codecs.is_empty() {
            return invalid(format!(
                "duplicate generated codecs registered for {:?}",
                self.duplicate_codecs
            ));
        }
        let source = self
            .artifacts
            .require(instance.instance_key())?
            .read_verified()?;
        if source.len() > self.limits.max_module_bytes {
            return exhausted("QuickJS Module Artifact exceeds max_module_bytes");
        }
        let source = String::from_utf8(source).map_err(|_| RuntimeFailure::ModuleFailure {
            detail: "QuickJS Module Artifact is not UTF-8 source".to_owned(),
        })?;
        let codecs = codecs_for_instance(instance, &self.codecs)?;
        let generation = Rc::new(QuickJsGeneration::load(
            instance.entrypoint(),
            &source,
            instance.clone(),
            self.limits.clone(),
        )?);
        let endpoints = json_request_endpoints(generation.clone(), codecs.clone());
        let stream_endpoints = json_stream_endpoints(generation.clone(), codecs);
        Ok(PreparedNativeModule::with_endpoints(
            endpoints,
            stream_endpoints,
            QuickJsLifecycle { generation },
        ))
    }
}

impl ExecutionAdapter for QuickJsAdapter {
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
            .module_instances()
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
    ) -> Result<PreparedNativeModule, RuntimeFailure> {
        let instance = plan.module_instance(instance_key).ok_or_else(|| {
            RuntimeFailure::InvalidResolvedPlan {
                detail: format!("unknown Instance `{instance_key}`"),
            }
        })?;
        if instance.execution_class().as_str() != EXECUTION_CLASS {
            return invalid(format!("Instance `{instance_key}` is not QuickJS"));
        }
        self.prepare_instance(instance)
    }
}

enum GuestCall {
    Invoke {
        capability: String,
        operation: String,
        payload: String,
    },
    StreamOpen {
        capability: String,
        operation: String,
        payload: String,
    },
    StreamSend {
        stream_id: u64,
        payload: String,
    },
    StreamReceive {
        stream_id: u64,
    },
    StreamCloseSend {
        stream_id: u64,
    },
    StreamCancel {
        stream_id: u64,
    },
}

struct GuestCommand {
    call: GuestCall,
    abandoned: Arc<AtomicBool>,
    outcome: futures::channel::oneshot::Sender<Result<JsonInvocationOutcome, String>>,
}

enum WorkerCommand {
    Call(GuestCommand),
    Shutdown,
}

struct QuickJsGeneration {
    commands: mpsc::SyncSender<WorkerCommand>,
    failed: Arc<AtomicBool>,
    interrupt: Arc<AtomicBool>,
    worker: RefCell<Option<thread::JoinHandle<()>>>,
    stopped: Cell<bool>,
    active_streams: Cell<usize>,
    max_streams: usize,
}

impl std::fmt::Debug for QuickJsGeneration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QuickJsGeneration")
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct InvocationGuard {
    started: Instant,
    abandoned: Arc<AtomicBool>,
}

impl QuickJsGeneration {
    fn load(
        entrypoint: &str,
        source: &str,
        instance: ModuleInstancePlan,
        limits: QuickJsLimits,
    ) -> Result<Self, RuntimeFailure> {
        let (commands, receiver) = mpsc::sync_channel(1);
        let (ready, ready_receiver) = mpsc::sync_channel(1);
        let failed = Arc::new(AtomicBool::new(false));
        let worker_failed = failed.clone();
        let interrupt = Arc::new(AtomicBool::new(false));
        let worker_interrupt = interrupt.clone();
        let entrypoint = entrypoint.to_owned();
        let source = source.to_owned();
        let max_streams = limits.max_streams;
        let worker = thread::Builder::new()
            .name("lenso-quickjs".to_owned())
            .spawn(move || {
                let inputs = QuickJsWorkerInputs {
                    entrypoint: &entrypoint,
                    source: &source,
                    instance: &instance,
                    limits: &limits,
                };
                let result = run_quickjs_worker(
                    inputs,
                    &receiver,
                    &worker_failed,
                    &worker_interrupt,
                    &ready,
                );
                if let Err(detail) = result {
                    let _ = ready.try_send(Err(detail));
                }
            })
            .map_err(quickjs_failure)?;
        match ready_receiver.recv() {
            Ok(Ok(())) => Ok(Self {
                commands,
                failed,
                interrupt,
                worker: RefCell::new(Some(worker)),
                stopped: Cell::new(false),
                active_streams: Cell::new(0),
                max_streams,
            }),
            Ok(Err(detail)) => {
                interrupt.store(true, Ordering::Release);
                let _ = worker.join();
                Err(RuntimeFailure::ModuleFailure { detail })
            }
            Err(error) => {
                interrupt.store(true, Ordering::Release);
                let _ = worker.join();
                Err(quickjs_failure(error))
            }
        }
    }

    async fn invoke_inner(
        &self,
        capability: String,
        operation: String,
        request_json: String,
        context: InvocationContext,
    ) -> Result<JsonInvocationOutcome, RuntimeFailure> {
        self.call_inner(
            GuestCall::Invoke {
                capability,
                operation,
                payload: request_json,
            },
            context,
            "invoke",
        )
        .await
    }

    async fn call_inner(
        &self,
        call: GuestCall,
        context: InvocationContext,
        operation_name: &'static str,
    ) -> Result<JsonInvocationOutcome, RuntimeFailure> {
        if self.failed.load(Ordering::Acquire) {
            return Err(RuntimeFailure::ModuleFailure {
                detail: "QuickJS generation is retired".to_owned(),
            });
        }
        let abandoned = Arc::new(AtomicBool::new(false));
        let mut abandonment = AbandonmentGuard(Some(abandoned.clone()));
        let (outcome, response) = futures::channel::oneshot::channel();
        self.commands
            .try_send(WorkerCommand::Call(GuestCommand {
                call,
                abandoned,
                outcome,
            }))
            .map_err(|_| RuntimeFailure::ResourceExhausted {
                capability: "lenso.quickjs@1",
                operation: operation_name.to_owned(),
            })?;
        let cancellation = context.cancellation();
        let mut response = response.fuse();
        let mut cancelled = cancellation.cancelled().fuse();
        select! {
            result = response => {
                abandonment.disarm();
                match result {
                    Ok(Ok(outcome)) => Ok(outcome),
                    Ok(Err(detail)) => {
                        self.failed.store(true, Ordering::Release);
                        Err(RuntimeFailure::ModuleFailure { detail: bounded(detail) })
                    }
                    Err(_) => {
                        self.failed.store(true, Ordering::Release);
                        Err(RuntimeFailure::ModuleFailure {
                            detail: "QuickJS worker stopped".to_owned(),
                        })
                    }
                }
            }
            () = cancelled => {
                self.failed.store(true, Ordering::Release);
                self.interrupt.store(true, Ordering::Release);
                Err(RuntimeFailure::Cancelled { request_id: context.request_id() })
            }
        }
    }

    fn stop(&self) {
        if self.stopped.replace(true) {
            return;
        }
        self.interrupt.store(true, Ordering::Release);
        let _ = self.commands.try_send(WorkerCommand::Shutdown);
        if let Some(worker) = self.worker.borrow_mut().take() {
            let _ = worker.join();
        }
    }

    fn reserve_stream(&self) -> Result<(), RuntimeFailure> {
        let active = self.active_streams.get();
        if active >= self.max_streams {
            return Err(RuntimeFailure::ResourceExhausted {
                capability: EXECUTION_CLASS,
                operation: "stream-open".to_owned(),
            });
        }
        self.active_streams.set(active + 1);
        Ok(())
    }

    fn release_stream(&self) {
        self.active_streams
            .set(self.active_streams.get().saturating_sub(1));
    }
}

impl JsonRequestTransport for QuickJsGeneration {
    fn invoke(
        self: Rc<Self>,
        capability: String,
        operation: String,
        request_json: String,
        context: InvocationContext,
    ) -> futures::future::LocalBoxFuture<'static, Result<JsonInvocationOutcome, RuntimeFailure>>
    {
        Box::pin(async move {
            self.invoke_inner(capability, operation, request_json, context)
                .await
        })
    }
}

impl JsonStreamTransport for QuickJsGeneration {
    fn open(
        self: Rc<Self>,
        capability: String,
        operation: String,
        request_json: String,
        context: InvocationContext,
    ) -> JsonStreamOpenFuture {
        Box::pin(async move {
            self.reserve_stream()?;
            let outcome = self
                .call_inner(
                    GuestCall::StreamOpen {
                        capability,
                        operation,
                        payload: request_json,
                    },
                    context.clone(),
                    "stream-open",
                )
                .await;
            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(error) => {
                    self.release_stream();
                    return Err(error);
                }
            };
            match outcome {
                JsonInvocationOutcome::Success(Value::Number(id)) => {
                    let stream_id = id.as_u64().ok_or(RuntimeFailure::ProtocolViolation {
                        capability: EXECUTION_CLASS,
                    })?;
                    Ok(Ok(Rc::new(QuickJsStreamSession {
                        generation: self,
                        stream_id,
                        context,
                        cancelled: Cell::new(false),
                        finished: Cell::new(false),
                    })
                        as Rc<dyn JsonStreamSessionTransport>))
                }
                JsonInvocationOutcome::Success(_) => {
                    self.release_stream();
                    Err(RuntimeFailure::ProtocolViolation {
                        capability: EXECUTION_CLASS,
                    })
                }
                JsonInvocationOutcome::DomainError(error) => {
                    self.release_stream();
                    Ok(Err(error))
                }
            }
        })
    }
}

#[derive(Debug)]
struct QuickJsStreamSession {
    generation: Rc<QuickJsGeneration>,
    stream_id: u64,
    context: InvocationContext,
    cancelled: Cell<bool>,
    finished: Cell<bool>,
}

impl QuickJsStreamSession {
    fn finish(&self) {
        if !self.finished.replace(true) {
            self.generation.release_stream();
        }
    }
    async fn call(
        &self,
        call: GuestCall,
        operation: &'static str,
    ) -> Result<JsonInvocationOutcome, RuntimeFailure> {
        if self.cancelled.get() {
            return Err(RuntimeFailure::Cancelled {
                request_id: self.context.request_id(),
            });
        }
        self.generation
            .call_inner(call, self.context.clone(), operation)
            .await
    }
}

impl JsonStreamSessionTransport for QuickJsStreamSession {
    fn send(
        self: Rc<Self>,
        message_json: String,
    ) -> futures::future::LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        Box::pin(async move {
            match self
                .call(
                    GuestCall::StreamSend {
                        stream_id: self.stream_id,
                        payload: message_json,
                    },
                    "stream-send",
                )
                .await?
            {
                JsonInvocationOutcome::Success(Value::Null) => Ok(()),
                _ => Err(RuntimeFailure::ProtocolViolation {
                    capability: EXECUTION_CLASS,
                }),
            }
        })
    }

    fn receive(
        self: Rc<Self>,
    ) -> futures::future::LocalBoxFuture<'static, Result<JsonStreamItem, RuntimeFailure>> {
        Box::pin(async move {
            match self
                .call(
                    GuestCall::StreamReceive {
                        stream_id: self.stream_id,
                    },
                    "stream-receive",
                )
                .await?
            {
                JsonInvocationOutcome::Success(value) => {
                    let frame: JsonStreamFrame = serde_json::from_value(value).map_err(|_| {
                        RuntimeFailure::ProtocolViolation {
                            capability: EXECUTION_CLASS,
                        }
                    })?;
                    Ok(match frame {
                        JsonStreamFrame::Message(value) => JsonStreamItem::Message(value),
                        JsonStreamFrame::PeerHalfClosed => JsonStreamItem::PeerHalfClosed,
                        JsonStreamFrame::TerminalSuccess => {
                            self.finish();
                            JsonStreamItem::Terminal(Ok(()))
                        }
                        JsonStreamFrame::TerminalError(value) => {
                            self.finish();
                            JsonStreamItem::Terminal(Err(value))
                        }
                    })
                }
                JsonInvocationOutcome::DomainError(_) => Err(RuntimeFailure::ProtocolViolation {
                    capability: EXECUTION_CLASS,
                }),
            }
        })
    }

    fn close_send(
        self: Rc<Self>,
    ) -> futures::future::LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        Box::pin(async move {
            match self
                .call(
                    GuestCall::StreamCloseSend {
                        stream_id: self.stream_id,
                    },
                    "stream-close-send",
                )
                .await?
            {
                JsonInvocationOutcome::Success(Value::Null) => Ok(()),
                _ => Err(RuntimeFailure::ProtocolViolation {
                    capability: EXECUTION_CLASS,
                }),
            }
        })
    }

    fn cancel(&self) {
        if self.cancelled.replace(true) {
            return;
        }
        self.finish();
        let abandoned = Arc::new(AtomicBool::new(false));
        let (outcome, _response) = futures::channel::oneshot::channel();
        let _ = self
            .generation
            .commands
            .try_send(WorkerCommand::Call(GuestCommand {
                call: GuestCall::StreamCancel {
                    stream_id: self.stream_id,
                },
                abandoned,
                outcome,
            }));
    }
}

impl Drop for QuickJsStreamSession {
    fn drop(&mut self) {
        self.finish();
    }
}

impl Drop for QuickJsGeneration {
    fn drop(&mut self) {
        self.stop();
    }
}

struct AbandonmentGuard(Option<Arc<AtomicBool>>);

impl AbandonmentGuard {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for AbandonmentGuard {
    fn drop(&mut self) {
        if let Some(abandoned) = &self.0 {
            abandoned.store(true, Ordering::Release);
        }
    }
}

#[derive(Clone, Copy)]
struct QuickJsWorkerInputs<'a> {
    entrypoint: &'a str,
    source: &'a str,
    instance: &'a ModuleInstancePlan,
    limits: &'a QuickJsLimits,
}

struct QuickJsExports {
    invoke: Persistent<Function<'static>>,
    stream_open: Option<Persistent<Function<'static>>>,
    stream_send: Option<Persistent<Function<'static>>>,
    stream_receive: Option<Persistent<Function<'static>>>,
    stream_close_send: Option<Persistent<Function<'static>>>,
    stream_cancel: Option<Persistent<Function<'static>>>,
}

#[allow(clippy::too_many_lines)]
fn run_quickjs_worker(
    inputs: QuickJsWorkerInputs<'_>,
    commands: &mpsc::Receiver<WorkerCommand>,
    failed: &Arc<AtomicBool>,
    interrupt: &Arc<AtomicBool>,
    ready: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let QuickJsWorkerInputs {
        entrypoint,
        source,
        instance,
        limits,
    } = inputs;
    let runtime = Runtime::new().map_err(|error| error.to_string())?;
    runtime.set_memory_limit(limits.max_heap_bytes);
    runtime.set_max_stack_size(limits.max_stack_bytes);
    let invocation = Rc::new(RefCell::new(Some(InvocationGuard {
        started: Instant::now(),
        abandoned: Arc::new(AtomicBool::new(false)),
    })));
    let interrupt_invocation = invocation.clone();
    let max_turn = limits.max_turn;
    let worker_interrupt = interrupt.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || {
        worker_interrupt.load(Ordering::Acquire)
            || interrupt_invocation.borrow().as_ref().is_some_and(|guard| {
                guard.abandoned.load(Ordering::Acquire) || guard.started.elapsed() >= max_turn
            })
    })));
    let context = Context::full(&runtime).map_err(|error| error.to_string())?;
    let requires_stream = instance
        .provided_capabilities()
        .iter()
        .any(|descriptor| !descriptor.stream_operations().is_empty());
    let (exports, descriptor) = context
        .with(|context| {
            harden_globals(&context)?;
            let (module, promise) = Module::declare(context.clone(), entrypoint, source)?.eval()?;
            finish_promise(&context, &promise, limits.max_pending_jobs)?;
            let describe: Function<'_> = module.get("describe")?;
            let descriptor: MaybePromise<'_> = describe.call(())?;
            let descriptor = finish_maybe_promise(&context, &descriptor, limits.max_pending_jobs)?;
            let invoke: Function<'_> = module.get("invoke")?;
            let stream_open = requires_stream
                .then(|| module.get::<_, Function<'_>>("streamOpen"))
                .transpose()?
                .map(|function| Persistent::save(&context, function));
            let stream_send = requires_stream
                .then(|| module.get::<_, Function<'_>>("streamSend"))
                .transpose()?
                .map(|function| Persistent::save(&context, function));
            let stream_receive = requires_stream
                .then(|| module.get::<_, Function<'_>>("streamReceive"))
                .transpose()?
                .map(|function| Persistent::save(&context, function));
            let stream_close_send = requires_stream
                .then(|| module.get::<_, Function<'_>>("streamCloseSend"))
                .transpose()?
                .map(|function| Persistent::save(&context, function));
            let stream_cancel = requires_stream
                .then(|| module.get::<_, Function<'_>>("streamCancel"))
                .transpose()?
                .map(|function| Persistent::save(&context, function));
            Ok::<_, rquickjs::Error>((
                QuickJsExports {
                    invoke: Persistent::save(&context, invoke),
                    stream_open,
                    stream_send,
                    stream_receive,
                    stream_close_send,
                    stream_cancel,
                },
                descriptor,
            ))
        })
        .map_err(|error| error.to_string())?;
    if descriptor.len() > limits.max_result_bytes {
        return Err("QuickJS descriptor exceeds max_result_bytes".to_owned());
    }
    validate_json_module_descriptor(instance, &descriptor)
        .map_err(|error| bounded(format!("QuickJS descriptor mismatch: {error:?}")))?;
    invocation.replace(None);
    ready.send(Ok(())).map_err(|error| error.to_string())?;
    while let Ok(command) = commands.recv() {
        match command {
            WorkerCommand::Shutdown => return Ok(()),
            WorkerCommand::Call(command) => {
                interrupt.store(false, Ordering::Release);
                invocation.replace(Some(InvocationGuard {
                    started: Instant::now(),
                    abandoned: command.abandoned,
                }));
                let result = context.with(|js| {
                    let promise: MaybePromise<'_> = match &command.call {
                        GuestCall::Invoke {
                            capability,
                            operation,
                            payload,
                        } => exports
                            .invoke
                            .clone()
                            .restore(&js)?
                            .call((capability, operation, payload))?,
                        GuestCall::StreamOpen {
                            capability,
                            operation,
                            payload,
                        } => exports
                            .stream_open
                            .as_ref()
                            .ok_or(rquickjs::Error::Exception)?
                            .clone()
                            .restore(&js)?
                            .call((capability, operation, payload))?,
                        GuestCall::StreamSend { stream_id, payload } => exports
                            .stream_send
                            .as_ref()
                            .ok_or(rquickjs::Error::Exception)?
                            .clone()
                            .restore(&js)?
                            .call((*stream_id, payload))?,
                        GuestCall::StreamReceive { stream_id } => exports
                            .stream_receive
                            .as_ref()
                            .ok_or(rquickjs::Error::Exception)?
                            .clone()
                            .restore(&js)?
                            .call((*stream_id,))?,
                        GuestCall::StreamCloseSend { stream_id } => exports
                            .stream_close_send
                            .as_ref()
                            .ok_or(rquickjs::Error::Exception)?
                            .clone()
                            .restore(&js)?
                            .call((*stream_id,))?,
                        GuestCall::StreamCancel { stream_id } => exports
                            .stream_cancel
                            .as_ref()
                            .ok_or(rquickjs::Error::Exception)?
                            .clone()
                            .restore(&js)?
                            .call((*stream_id,))?,
                    };
                    finish_maybe_promise(&js, &promise, limits.max_pending_jobs)
                });
                invocation.replace(None);
                let outcome = result
                    .map_err(|error| format!("QuickJS invocation failed: {error}"))
                    .and_then(|encoded| decode_quickjs_result(&encoded, limits));
                if outcome.is_err() {
                    failed.store(true, Ordering::Release);
                }
                let _ = command.outcome.send(outcome);
                if failed.load(Ordering::Acquire) {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

fn finish_promise(
    context: &rquickjs::Ctx<'_>,
    promise: &rquickjs::Promise<'_>,
    max_jobs: usize,
) -> rquickjs::Result<()> {
    for completed_jobs in 0..=max_jobs {
        if let Some(result) = promise.result::<()>() {
            return result;
        }
        if completed_jobs == max_jobs {
            break;
        }
        if !context.execute_pending_job() {
            return Err(rquickjs::Error::Exception);
        }
    }
    Err(rquickjs::Error::Exception)
}

fn finish_maybe_promise(
    context: &rquickjs::Ctx<'_>,
    promise: &MaybePromise<'_>,
    max_jobs: usize,
) -> rquickjs::Result<String> {
    for completed_jobs in 0..=max_jobs {
        if let Some(result) = promise.result::<String>() {
            return result;
        }
        if completed_jobs == max_jobs {
            break;
        }
        if !context.execute_pending_job() {
            return Err(rquickjs::Error::Exception);
        }
    }
    Err(rquickjs::Error::Exception)
}

fn decode_quickjs_result(
    encoded: &str,
    limits: &QuickJsLimits,
) -> Result<JsonInvocationOutcome, String> {
    if encoded.len() > limits.max_result_bytes {
        return Err("QuickJS result exceeds max_result_bytes".to_owned());
    }
    let envelope: Value = serde_json::from_str(encoded)
        .map_err(|error| format!("QuickJS returned invalid JSON: {error}"))?;
    decode_envelope(envelope).map_err(|error| format!("{error:?}"))
}

#[derive(Debug)]
struct QuickJsLifecycle {
    generation: Rc<QuickJsGeneration>,
}

impl ModuleLifecycle for QuickJsLifecycle {
    fn deactivate(&self, _context: lenso_kernel::DeactivateContext) -> lenso_kernel::ModuleFuture {
        self.generation.stop();
        Box::pin(futures::future::ready(Ok(())))
    }
}

fn harden_globals(context: &rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    context.eval::<(), _>(
        r#"
        globalThis.eval = undefined;
        globalThis.Function = undefined;
        globalThis.Date = undefined;
        Object.defineProperty(Math, "random", { value: undefined, writable: false });
        "#,
    )
}

fn decode_envelope(value: Value) -> Result<JsonInvocationOutcome, RuntimeFailure> {
    let Value::Object(mut object) = value else {
        return Err(RuntimeFailure::ModuleFailure {
            detail: "QuickJS result envelope is not an object".to_owned(),
        });
    };
    match (
        object.remove("ok"),
        object.remove("error"),
        object.is_empty(),
    ) {
        (Some(value), None, true) => Ok(JsonInvocationOutcome::Success(value)),
        (None, Some(value), true) => Ok(JsonInvocationOutcome::DomainError(value)),
        _ => Err(RuntimeFailure::ModuleFailure {
            detail: "QuickJS result envelope must contain exactly one of `ok` or `error`"
                .to_owned(),
        }),
    }
}

fn quickjs_failure(error: impl std::fmt::Display) -> RuntimeFailure {
    RuntimeFailure::ModuleFailure {
        detail: bounded(format!("QuickJS generation failure: {error}")),
    }
}

fn invalid<T>(detail: String) -> Result<T, RuntimeFailure> {
    Err(RuntimeFailure::InvalidResolvedPlan { detail })
}

fn exhausted<T>(detail: &str) -> Result<T, RuntimeFailure> {
    Err(RuntimeFailure::ModuleFailure {
        detail: detail.to_owned(),
    })
}

fn bounded(mut detail: String) -> String {
    detail.truncate(1024);
    detail
}
