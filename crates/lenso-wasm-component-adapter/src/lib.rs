//! Bounded Wasmtime Component Model Execution Adapter.

use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use futures::{FutureExt, select};
use lenso_app_plan::{ExecutionClassId, ModuleInstancePlan, ResolvedAppPlan};
use lenso_kernel::{
    ExecutionAdapter, InvocationContext, ModuleLifecycle, PreparedNativeApp, PreparedNativeModule,
    RuntimeFailure,
};
use lenso_runtime_codec::{
    ArtifactCatalog, JsonCapabilityCodec, JsonInvocationOutcome, JsonRequestTransport,
    codecs_for_instance, json_request_endpoints, prepare_request_app,
    validate_json_module_descriptor,
};
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};

wasmtime::component::bindgen!({
    inline: r#"
        package lenso:runtime@1.0.0;

        world plugin {
            export describe: func() -> string;
            export invoke: func(capability: string, operation: string, request-json: string) -> result<string, string>;
        }
    "#,
    world: "plugin",
});

/// Stable open execution-class identity.
pub const EXECUTION_CLASS: &str = "lenso.wasm-component@1";

/// Per-generation Wasmtime resource and execution limits.
#[derive(Clone, Debug)]
pub struct WasmComponentLimits {
    pub max_component_bytes: usize,
    pub max_memory_bytes: usize,
    pub max_table_elements: usize,
    pub max_instances: usize,
    pub max_result_bytes: usize,
    pub fuel_per_invocation: u64,
    pub max_turn: Duration,
}

impl Default for WasmComponentLimits {
    fn default() -> Self {
        Self {
            max_component_bytes: 16 * 1024 * 1024,
            max_memory_bytes: 64 * 1024 * 1024,
            max_table_elements: 10_000,
            max_instances: 16,
            max_result_bytes: 1024 * 1024,
            fuel_per_invocation: 10_000_000,
            max_turn: Duration::from_secs(1),
        }
    }
}

/// Wasmtime Component Adapter with no WASI or ambient host imports.
#[derive(Debug)]
pub struct WasmComponentAdapter {
    artifacts: ArtifactCatalog,
    codecs: BTreeMap<String, Rc<dyn JsonCapabilityCodec>>,
    duplicate_codecs: BTreeSet<String>,
    limits: WasmComponentLimits,
}

impl WasmComponentAdapter {
    /// Creates one Adapter for a resolved Generation Artifact catalog.
    pub fn new(artifacts: ArtifactCatalog) -> Self {
        Self {
            artifacts,
            codecs: BTreeMap::new(),
            duplicate_codecs: BTreeSet::new(),
            limits: WasmComponentLimits::default(),
        }
    }

    /// Registers one generated Capability codec.
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

    /// Applies host-policy limits.
    #[must_use]
    pub fn with_limits(mut self, limits: WasmComponentLimits) -> Self {
        self.limits = limits;
        self
    }

    fn prepare_instance(
        &self,
        instance: &ModuleInstancePlan,
    ) -> Result<PreparedNativeModule, RuntimeFailure> {
        if instance.entrypoint() != "plugin" {
            return invalid(format!(
                "Wasm Component Instance `{}` requires the `plugin` world entrypoint",
                instance.instance_key()
            ));
        }
        if !self.duplicate_codecs.is_empty() {
            return invalid(format!(
                "duplicate generated codecs registered for {:?}",
                self.duplicate_codecs
            ));
        }
        let bytes = self
            .artifacts
            .require(instance.instance_key())?
            .read_verified()?;
        if bytes.len() > self.limits.max_component_bytes {
            return module_failure("Wasm Component exceeds max_component_bytes");
        }
        let codecs = codecs_for_instance(instance, &self.codecs)?;
        let generation = Rc::new(WasmGeneration::start(
            bytes,
            instance.clone(),
            self.limits.clone(),
        )?);
        let endpoints = json_request_endpoints(generation.clone(), codecs);
        Ok(PreparedNativeModule::new(
            endpoints,
            WasmLifecycle { generation },
        ))
    }
}

impl ExecutionAdapter for WasmComponentAdapter {
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
            return invalid(format!("Instance `{instance_key}` is not a Wasm Component"));
        }
        self.prepare_instance(instance)
    }
}

struct InvokeCommand {
    capability: String,
    operation: String,
    request_json: String,
    abandoned: Arc<AtomicBool>,
    outcome: futures::channel::oneshot::Sender<Result<JsonInvocationOutcome, String>>,
}

enum WorkerCommand {
    Invoke(InvokeCommand),
    Shutdown,
}

enum DeadlineCommand {
    Arm(std::time::Duration),
    Disarm,
    Shutdown,
}

struct WasmGeneration {
    commands: mpsc::SyncSender<WorkerCommand>,
    engine: Engine,
    failed: Arc<AtomicBool>,
    worker: std::cell::RefCell<Option<thread::JoinHandle<()>>>,
    stopped: std::cell::Cell<bool>,
}

impl std::fmt::Debug for WasmGeneration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WasmGeneration")
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

impl WasmGeneration {
    fn start(
        bytes: Vec<u8>,
        instance: ModuleInstancePlan,
        limits: WasmComponentLimits,
    ) -> Result<Self, RuntimeFailure> {
        let mut config = Config::new();
        config
            .wasm_component_model(true)
            .consume_fuel(true)
            .epoch_interruption(true)
            .max_wasm_stack(1024 * 1024);
        let engine = Engine::new(&config).map_err(wasm_failure)?;
        let component = Component::new(&engine, bytes).map_err(wasm_failure)?;
        let linker = Linker::<HostState>::new(&engine);
        let (commands, receiver) = mpsc::sync_channel::<WorkerCommand>(1);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker_engine = engine.clone();
        let failed = Arc::new(AtomicBool::new(false));
        let worker_failed = failed.clone();
        let worker = thread::Builder::new()
            .name("lenso-wasm-component".to_owned())
            .spawn(move || {
                let inputs = WasmWorkerInputs {
                    engine: &worker_engine,
                    component: &component,
                    linker: &linker,
                    instance: &instance,
                    limits: &limits,
                };
                let result = run_worker(inputs, &receiver, &worker_failed, &ready_tx);
                if let Err(detail) = result {
                    let _ = ready_tx.try_send(Err(detail));
                }
            })
            .map_err(wasm_failure)?;
        match ready_rx.recv() {
            Err(error) => {
                engine.increment_epoch();
                let _ = worker.join();
                Err(wasm_failure(error))
            }
            Ok(Ok(())) => Ok(Self {
                commands,
                engine,
                failed,
                worker: std::cell::RefCell::new(Some(worker)),
                stopped: std::cell::Cell::new(false),
            }),
            Ok(Err(detail)) => {
                engine.increment_epoch();
                let _ = worker.join();
                module_failure(detail)
            }
        }
    }

    fn stop(&self) {
        if self.stopped.replace(true) {
            return;
        }
        let _ = self.commands.try_send(WorkerCommand::Shutdown);
        self.engine.increment_epoch();
        if let Some(worker) = self.worker.borrow_mut().take() {
            let _ = worker.join();
        }
    }

    async fn invoke_inner(
        &self,
        capability: String,
        operation: String,
        request_json: String,
        context: InvocationContext,
    ) -> Result<JsonInvocationOutcome, RuntimeFailure> {
        if self.failed.load(Ordering::Acquire) {
            return Err(RuntimeFailure::ModuleFailure {
                detail: "Wasm Component generation is retired".to_owned(),
            });
        }
        let abandoned = Arc::new(AtomicBool::new(false));
        let mut abandonment = WasmAbandonmentGuard::new(abandoned.clone(), self.engine.clone());
        let (outcome, response) = futures::channel::oneshot::channel();
        self.commands
            .try_send(WorkerCommand::Invoke(InvokeCommand {
                capability,
                operation,
                request_json,
                abandoned,
                outcome,
            }))
            .map_err(|_| RuntimeFailure::ResourceExhausted {
                capability: "lenso.wasm-component@1",
                operation: "invoke".to_owned(),
            })?;
        let cancellation = context.cancellation();
        let mut response = response.fuse();
        let mut cancelled = cancellation.cancelled().fuse();
        select! {
            result = response => match result {
                Ok(Ok(outcome)) => {
                    abandonment.disarm();
                    Ok(outcome)
                },
                Ok(Err(detail)) => {
                    self.failed.store(true, Ordering::Release);
                    Err(RuntimeFailure::ModuleFailure { detail: bounded(detail) })
                }
                Err(_) => {
                    self.failed.store(true, Ordering::Release);
                    Err(RuntimeFailure::ModuleFailure {
                        detail: "Wasm Component worker stopped".to_owned(),
                    })
                }
            },
            () = cancelled => {
                self.failed.store(true, Ordering::Release);
                self.engine.increment_epoch();
                Err(RuntimeFailure::Cancelled { request_id: context.request_id() })
            }
        }
    }
}

impl JsonRequestTransport for WasmGeneration {
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

impl Drop for WasmGeneration {
    fn drop(&mut self) {
        self.stop();
    }
}

struct WasmAbandonmentGuard {
    abandoned: Option<Arc<AtomicBool>>,
    engine: Engine,
}

impl WasmAbandonmentGuard {
    fn new(abandoned: Arc<AtomicBool>, engine: Engine) -> Self {
        Self {
            abandoned: Some(abandoned),
            engine,
        }
    }

    fn disarm(&mut self) {
        self.abandoned = None;
    }
}

impl Drop for WasmAbandonmentGuard {
    fn drop(&mut self) {
        if let Some(abandoned) = &self.abandoned {
            abandoned.store(true, Ordering::Release);
            self.engine.increment_epoch();
        }
    }
}

#[derive(Debug)]
struct HostState {
    limits: StoreLimits,
}

#[derive(Clone, Copy)]
struct WasmWorkerInputs<'a> {
    engine: &'a Engine,
    component: &'a Component,
    linker: &'a Linker<HostState>,
    instance: &'a ModuleInstancePlan,
    limits: &'a WasmComponentLimits,
}

#[allow(clippy::too_many_lines)]
fn run_worker(
    inputs: WasmWorkerInputs<'_>,
    receiver: &mpsc::Receiver<WorkerCommand>,
    failed: &Arc<AtomicBool>,
    ready: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let WasmWorkerInputs {
        engine,
        component,
        linker,
        instance,
        limits,
    } = inputs;
    let store_limits = StoreLimitsBuilder::new()
        .memory_size(limits.max_memory_bytes)
        .table_elements(limits.max_table_elements)
        .instances(limits.max_instances)
        .memories(limits.max_instances)
        .tables(limits.max_instances)
        .trap_on_grow_failure(true)
        .build();
    let mut store = Store::new(
        engine,
        HostState {
            limits: store_limits,
        },
    );
    store.limiter(|state| &mut state.limits);
    store
        .set_fuel(limits.fuel_per_invocation)
        .map_err(|error| error.to_string())?;
    store.set_epoch_deadline(1);
    store.epoch_deadline_trap();
    let (deadline_tx, deadline_rx) = mpsc::channel();
    let deadline_engine = engine.clone();
    let deadline_worker = thread::Builder::new()
        .name("lenso-wasm-deadline".to_owned())
        .spawn(move || run_deadline_worker(&deadline_engine, &deadline_rx))
        .map_err(|error| error.to_string())?;
    deadline_tx
        .send(DeadlineCommand::Arm(limits.max_turn))
        .map_err(|error| error.to_string())?;
    let bindings = Plugin::instantiate(&mut store, component, linker)
        .map_err(|error| bounded(error.to_string()))?;
    let descriptor = bindings
        .call_describe(&mut store)
        .map_err(|error| bounded(format!("Wasm Component describe trapped: {error}")))?;
    if descriptor.len() > limits.max_result_bytes {
        return Err("Wasm Component descriptor exceeds max_result_bytes".to_owned());
    }
    validate_json_module_descriptor(instance, &descriptor)
        .map_err(|error| bounded(format!("Wasm Component descriptor mismatch: {error:?}")))?;
    deadline_tx
        .send(DeadlineCommand::Disarm)
        .map_err(|error| error.to_string())?;
    ready.send(Ok(())).map_err(|error| error.to_string())?;
    let worker_result = (|| {
        while let Ok(command) = receiver.recv() {
            match command {
                WorkerCommand::Shutdown => return Ok(()),
                WorkerCommand::Invoke(command) => {
                    if command.abandoned.load(Ordering::Acquire) {
                        failed.store(true, Ordering::Release);
                        let _ = command
                            .outcome
                            .send(Err("Wasm Component invocation was abandoned".to_owned()));
                        return Ok(());
                    }
                    store
                        .set_fuel(limits.fuel_per_invocation)
                        .map_err(|error| error.to_string())?;
                    store.set_epoch_deadline(1);
                    store.epoch_deadline_trap();
                    deadline_tx
                        .send(DeadlineCommand::Arm(limits.max_turn))
                        .map_err(|error| error.to_string())?;
                    let result = bindings.call_invoke(
                        &mut store,
                        &command.capability,
                        &command.operation,
                        &command.request_json,
                    );
                    deadline_tx
                        .send(DeadlineCommand::Disarm)
                        .map_err(|error| error.to_string())?;
                    let outcome = match result {
                        Ok(Ok(value)) if value.len() <= limits.max_result_bytes => {
                            serde_json::from_str(&value)
                                .map(JsonInvocationOutcome::Success)
                                .map_err(|error| {
                                    format!("invalid Component response JSON: {error}")
                                })
                        }
                        Ok(Err(value)) if value.len() <= limits.max_result_bytes => {
                            serde_json::from_str(&value)
                                .map(JsonInvocationOutcome::DomainError)
                                .map_err(|error| {
                                    format!("invalid Component Domain Error JSON: {error}")
                                })
                        }
                        Ok(_) => Err("Component result exceeds max_result_bytes".to_owned()),
                        Err(error) => Err(format!("Wasm Component trapped: {error}")),
                    };
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
    })();
    let _ = deadline_tx.send(DeadlineCommand::Shutdown);
    let _ = deadline_worker.join();
    worker_result
}

fn run_deadline_worker(engine: &Engine, commands: &mpsc::Receiver<DeadlineCommand>) {
    while let Ok(command) = commands.recv() {
        match command {
            DeadlineCommand::Arm(duration) => loop {
                match commands.recv_timeout(duration) {
                    Ok(DeadlineCommand::Disarm) => break,
                    Ok(DeadlineCommand::Arm(next_duration)) => {
                        if next_duration != duration {
                            // Only the single Wasm worker can arm this timer, so this branch is a
                            // defensive reset rather than concurrent invocation support.
                            break;
                        }
                    }
                    Ok(DeadlineCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        engine.increment_epoch();
                        break;
                    }
                }
            },
            DeadlineCommand::Disarm => {}
            DeadlineCommand::Shutdown => return,
        }
    }
}

#[derive(Debug)]
struct WasmLifecycle {
    generation: Rc<WasmGeneration>,
}

impl ModuleLifecycle for WasmLifecycle {
    fn deactivate(&self, _context: lenso_kernel::DeactivateContext) -> lenso_kernel::ModuleFuture {
        self.generation.stop();
        Box::pin(futures::future::ready(Ok(())))
    }
}

fn wasm_failure(error: impl std::fmt::Display) -> RuntimeFailure {
    RuntimeFailure::ModuleFailure {
        detail: bounded(format!("Wasm Component generation failure: {error}")),
    }
}

fn module_failure<T>(detail: impl Into<String>) -> Result<T, RuntimeFailure> {
    Err(RuntimeFailure::ModuleFailure {
        detail: bounded(detail.into()),
    })
}

fn invalid<T>(detail: String) -> Result<T, RuntimeFailure> {
    Err(RuntimeFailure::InvalidResolvedPlan { detail })
}

fn bounded(mut detail: String) -> String {
    detail.truncate(1024);
    detail
}
