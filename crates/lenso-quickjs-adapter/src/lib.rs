//! Bounded QuickJS-NG Execution Adapter for immutable bundled ES modules.

use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::BTreeMap,
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
    ExecutionAdapter, InvocationContext, ModuleLifecycle, NativeRequestEndpoint, PreparedNativeApp,
    PreparedNativeModule, RuntimeFailure,
};
use lenso_runtime_codec::{
    ArtifactCatalog, JsonCapabilityCodec, JsonInvocationOutcome, codecs_for_instance,
    prepare_request_app,
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
            max_turn: Duration::from_secs(1),
        }
    }
}

/// QuickJS-NG Adapter configured with exact Artifact handles and generated codecs.
#[derive(Debug)]
pub struct QuickJsAdapter {
    artifacts: ArtifactCatalog,
    codecs: BTreeMap<String, Rc<dyn JsonCapabilityCodec>>,
    limits: QuickJsLimits,
}

impl QuickJsAdapter {
    /// Creates one Adapter for a resolved Generation Artifact catalog.
    pub fn new(artifacts: ArtifactCatalog) -> Self {
        Self {
            artifacts,
            codecs: BTreeMap::new(),
            limits: QuickJsLimits::default(),
        }
    }

    /// Registers the generated codec for one exact Capability Descriptor.
    #[must_use]
    pub fn with_codec(mut self, codec: impl JsonCapabilityCodec) -> Self {
        self.codecs
            .insert(codec.capability_id().to_owned(), Rc::new(codec));
        self
    }

    /// Registers an already shared generated Capability codec.
    #[must_use]
    pub fn with_shared_codec(mut self, codec: Rc<dyn JsonCapabilityCodec>) -> Self {
        self.codecs.insert(codec.capability_id().to_owned(), codec);
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
            self.limits.clone(),
        )?);
        let endpoints = codecs
            .into_iter()
            .map(|codec| {
                Rc::new(QuickJsEndpoint {
                    generation: generation.clone(),
                    codec,
                }) as Rc<dyn NativeRequestEndpoint>
            })
            .collect();
        Ok(PreparedNativeModule::new(
            endpoints,
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

struct InvokeCommand {
    operation: String,
    request_json: String,
    abandoned: Arc<AtomicBool>,
    outcome: futures::channel::oneshot::Sender<Result<JsonInvocationOutcome, String>>,
}

enum WorkerCommand {
    Invoke(InvokeCommand),
    Shutdown,
}

struct QuickJsGeneration {
    commands: mpsc::SyncSender<WorkerCommand>,
    failed: Arc<AtomicBool>,
    interrupt: Arc<AtomicBool>,
    worker: RefCell<Option<thread::JoinHandle<()>>>,
    stopped: Cell<bool>,
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
    fn load(entrypoint: &str, source: &str, limits: QuickJsLimits) -> Result<Self, RuntimeFailure> {
        let (commands, receiver) = mpsc::sync_channel(1);
        let (ready, ready_receiver) = mpsc::sync_channel(1);
        let failed = Arc::new(AtomicBool::new(false));
        let worker_failed = failed.clone();
        let interrupt = Arc::new(AtomicBool::new(false));
        let worker_interrupt = interrupt.clone();
        let entrypoint = entrypoint.to_owned();
        let source = source.to_owned();
        let worker = thread::Builder::new()
            .name("lenso-quickjs".to_owned())
            .spawn(move || {
                let result = run_quickjs_worker(
                    &entrypoint,
                    &source,
                    &limits,
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

    async fn invoke(
        &self,
        operation: String,
        request_json: String,
        context: InvocationContext,
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
            .try_send(WorkerCommand::Invoke(InvokeCommand {
                operation,
                request_json,
                abandoned,
                outcome,
            }))
            .map_err(|_| RuntimeFailure::ResourceExhausted {
                capability: "lenso.quickjs@1",
                operation: "invoke".to_owned(),
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

fn run_quickjs_worker(
    entrypoint: &str,
    source: &str,
    limits: &QuickJsLimits,
    commands: &mpsc::Receiver<WorkerCommand>,
    failed: &Arc<AtomicBool>,
    interrupt: &Arc<AtomicBool>,
    ready: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
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
    let invoke = context
        .with(|context| {
            harden_globals(&context)?;
            let (module, promise) = Module::declare(context.clone(), entrypoint, source)?.eval()?;
            finish_promise(&context, &promise, limits.max_pending_jobs)?;
            let invoke: Function<'_> = module.get("invoke")?;
            Ok::<_, rquickjs::Error>(Persistent::save(&context, invoke))
        })
        .map_err(|error| error.to_string())?;
    invocation.replace(None);
    ready.send(Ok(())).map_err(|error| error.to_string())?;
    while let Ok(command) = commands.recv() {
        match command {
            WorkerCommand::Shutdown => return Ok(()),
            WorkerCommand::Invoke(command) => {
                interrupt.store(false, Ordering::Release);
                invocation.replace(Some(InvocationGuard {
                    started: Instant::now(),
                    abandoned: command.abandoned,
                }));
                let result = context.with(|js| {
                    let function = invoke.clone().restore(&js)?;
                    let promise: MaybePromise<'_> =
                        function.call((&command.operation, &command.request_json))?;
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
struct QuickJsEndpoint {
    generation: Rc<QuickJsGeneration>,
    codec: Rc<dyn JsonCapabilityCodec>,
}

impl NativeRequestEndpoint for QuickJsEndpoint {
    fn capability_id(&self) -> &'static str {
        self.codec.capability_id()
    }

    fn descriptor_version(&self) -> &'static str {
        self.codec.descriptor_version()
    }

    fn operations(&self) -> &'static [&'static str] {
        self.codec.request_operations()
    }

    fn invoke(
        &self,
        operation: &str,
        request: Box<dyn Any>,
        context: InvocationContext,
    ) -> futures::future::LocalBoxFuture<
        'static,
        Result<Result<Box<dyn Any>, Box<dyn Any>>, RuntimeFailure>,
    > {
        let codec = self.codec.clone();
        let generation = self.generation.clone();
        let operation = operation.to_owned();
        Box::pin(async move {
            if !codec.request_operations().contains(&operation.as_str()) {
                return Err(RuntimeFailure::UnknownOperation {
                    capability: codec.capability_id(),
                    operation,
                });
            }
            let request = codec.encode_request(&operation, request.as_ref())?;
            let request =
                serde_json::to_string(&request).map_err(|_| RuntimeFailure::ProtocolViolation {
                    capability: codec.capability_id(),
                })?;
            match generation
                .invoke(operation.clone(), request, context)
                .await?
            {
                JsonInvocationOutcome::Success(value) => {
                    codec.decode_response(&operation, value).map(Ok)
                }
                JsonInvocationOutcome::DomainError(value) => {
                    codec.decode_domain_error(&operation, value).map(Err)
                }
            }
        })
    }
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
