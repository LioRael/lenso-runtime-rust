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

use futures::{FutureExt, StreamExt, channel::mpsc as futures_mpsc, select};
use lenso_app_plan::{ExecutionClassId, PluginInstancePlan, ResolvedAppPlan};
use lenso_kernel::{
    ExecutionAdapter, InvocationContext, PluginLifecycle, PreparedNativeApp, PreparedNativePlugin,
    RuntimeFailure,
};
use lenso_runtime_codec::{
    ArtifactCatalog, JSON_HOST_IMPORTS_ABI_V2, JsonCapabilityCodec, JsonHostImports,
    JsonInvocationOutcome, JsonRequestTransport, JsonStreamFrame, JsonStreamItem,
    JsonStreamOpenFuture, JsonStreamSessionTransport, JsonStreamTransport, codecs_for_instance,
    codecs_for_requirements, json_host_invocation_envelope, json_request_endpoints,
    json_runtime_failure, json_stream_endpoints, prepare_request_app,
    validate_json_plugin_descriptor,
};
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};

mod request_abi {
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
}

mod interactions_abi {
    wasmtime::component::bindgen!({
        inline: r#"
            package lenso:runtime@1.0.0;
            world plugin {
                export describe: func() -> string;
                export invoke: func(capability: string, operation: string, request-json: string) -> result<string, string>;
                export stream-open: func(capability: string, operation: string, request-json: string) -> result<u64, string>;
                export stream-send: func(stream-id: u64, message-json: string) -> result<_, string>;
                export stream-receive: func(stream-id: u64) -> result<string, string>;
                export stream-close-send: func(stream-id: u64) -> result<_, string>;
                export stream-cancel: func(stream-id: u64);
            }
        "#,
        world: "plugin",
    });
}

mod host_imports_abi {
    wasmtime::component::bindgen!({
        inline: r#"
            package lenso:runtime@1.0.0;
            world plugin {
                import host-bindings: func() -> string;
                import host-invoke: func(binding-id: u32, operation: string, request-json: string) -> string;
                import host-stream-open: func(binding-id: u32, operation: string, request-json: string) -> string;
                import host-stream-send: func(stream-id: u64, message-json: string) -> string;
                import host-stream-receive: func(stream-id: u64) -> string;
                import host-stream-close-send: func(stream-id: u64) -> string;
                import host-stream-cancel: func(stream-id: u64) -> string;
                export describe: func() -> string;
                export invoke: func(capability: string, operation: string, request-json: string) -> result<string, string>;
                export stream-open: func(capability: string, operation: string, request-json: string) -> result<u64, string>;
                export stream-send: func(stream-id: u64, message-json: string) -> result<_, string>;
                export stream-receive: func(stream-id: u64) -> result<string, string>;
                export stream-close-send: func(stream-id: u64) -> result<_, string>;
                export stream-cancel: func(stream-id: u64);
            }
        "#,
        world: "plugin",
    });
}

/// Stable open execution-class identity.
pub const EXECUTION_CLASS: &str = "lenso.wasm-component@1";
/// Exact runtime profile implemented by this Adapter release.
pub const RUNTIME_PROFILE: &str = "lenso.wasm-component@1";

/// Per-generation Wasmtime resource and execution limits.
#[derive(Clone, Debug)]
pub struct WasmComponentLimits {
    pub max_component_bytes: usize,
    pub max_memory_bytes: usize,
    pub max_table_elements: usize,
    pub max_instances: usize,
    pub max_result_bytes: usize,
    pub max_streams: usize,
    pub max_host_imports_per_call: usize,
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
            max_streams: 1024,
            max_host_imports_per_call: 1024,
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
        instance: &PluginInstancePlan,
    ) -> Result<PreparedNativePlugin, RuntimeFailure> {
        if instance.runtime_profile() != RUNTIME_PROFILE {
            return invalid(format!(
                "Wasm Component Adapter does not support runtime profile `{}`",
                instance.runtime_profile()
            ));
        }
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
            return plugin_failure("Wasm Component exceeds max_component_bytes");
        }
        let codecs = codecs_for_instance(instance, &self.codecs)?;
        let import_codecs = codecs_for_requirements(instance, &self.codecs)?;
        let generation = Rc::new(WasmGeneration::start(
            bytes,
            instance.clone(),
            import_codecs,
            self.limits.clone(),
        )?);
        let endpoints = json_request_endpoints(generation.clone(), codecs.clone());
        let stream_endpoints = json_stream_endpoints(generation.clone(), codecs);
        Ok(PreparedNativePlugin::with_endpoints(
            endpoints,
            stream_endpoints,
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
            return invalid(format!("Instance `{instance_key}` is not a Wasm Component"));
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

enum HostImportCall {
    Bindings,
    Invoke {
        binding_id: u32,
        operation: String,
        payload: String,
    },
    StreamOpen {
        binding_id: u32,
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

struct HostImportCommand {
    call: HostImportCall,
    response: mpsc::SyncSender<String>,
}

struct GuestCommand {
    call: GuestCall,
    abandoned: Arc<AtomicBool>,
    imports: futures_mpsc::Sender<HostImportCommand>,
    outcome: futures::channel::oneshot::Sender<Result<JsonInvocationOutcome, String>>,
}

enum WorkerCommand {
    Call(GuestCommand),
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
    active_streams: std::cell::Cell<usize>,
    max_streams: usize,
    max_host_imports_per_call: usize,
    host_imports: Rc<JsonHostImports>,
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
        instance: PluginInstancePlan,
        import_codecs: Vec<Rc<dyn JsonCapabilityCodec>>,
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
        let mut linker = Linker::<HostState>::new(&engine);
        host_imports_abi::Plugin::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
            .map_err(wasm_failure)?;
        let (commands, receiver) = mpsc::sync_channel::<WorkerCommand>(1);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker_engine = engine.clone();
        let max_streams = limits.max_streams;
        let max_host_imports_per_call = limits.max_host_imports_per_call;
        let host_imports = Rc::new(JsonHostImports::new(import_codecs, limits.max_streams)?);
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
                active_streams: std::cell::Cell::new(0),
                max_streams,
                max_host_imports_per_call,
                host_imports,
            }),
            Ok(Err(detail)) => {
                engine.increment_epoch();
                let _ = worker.join();
                plugin_failure(detail)
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
            return Err(RuntimeFailure::PluginFailure {
                detail: "Wasm Component generation is retired".to_owned(),
            });
        }
        let abandoned = Arc::new(AtomicBool::new(false));
        let mut abandonment = WasmAbandonmentGuard::new(abandoned.clone(), self.engine.clone());
        let (outcome, response) = futures::channel::oneshot::channel();
        let (imports, import_receiver) = futures_mpsc::channel(1);
        self.commands
            .try_send(WorkerCommand::Call(GuestCommand {
                call,
                abandoned,
                imports,
                outcome,
            }))
            .map_err(|_| RuntimeFailure::ResourceExhausted {
                capability: "lenso.wasm-component@1",
                operation: operation_name.to_owned(),
            })?;
        let cancellation = context.cancellation();
        let mut response = response.fuse();
        let mut import_receiver = import_receiver.fuse();
        let mut cancelled = cancellation.cancelled().fuse();
        let mut imported = 0_usize;
        loop {
            select! {
                result = response => {
                    abandonment.disarm();
                    return match result {
                        Ok(Ok(outcome)) => Ok(outcome),
                        Ok(Err(detail)) => {
                            self.failed.store(true, Ordering::Release);
                            Err(RuntimeFailure::PluginFailure { detail: bounded(detail) })
                        }
                        Err(_) => {
                            self.failed.store(true, Ordering::Release);
                            Err(RuntimeFailure::PluginFailure {
                                detail: "Wasm Component worker stopped".to_owned(),
                            })
                        }
                    };
                }
                command = import_receiver.next() => {
                    let Some(command) = command else {
                        continue;
                    };
                    imported = imported.saturating_add(1);
                    let encoded = if imported > self.max_host_imports_per_call {
                        serde_json::to_string(&serde_json::json!({
                            "runtime": json_runtime_failure(&RuntimeFailure::ResourceExhausted {
                                capability: JSON_HOST_IMPORTS_ABI_V2,
                                operation: "invoke".to_owned(),
                            })
                        }))
                        .expect("host import Runtime Failure is JSON")
                    } else {
                        self.dispatch_host_import(command.call, context.clone()).await
                    };
                    let _ = command.response.send(encoded);
                }
                () = cancelled => {
                    self.failed.store(true, Ordering::Release);
                    self.engine.increment_epoch();
                    return Err(RuntimeFailure::Cancelled { request_id: context.request_id() });
                }
            }
        }
    }

    async fn dispatch_host_import(
        &self,
        call: HostImportCall,
        context: InvocationContext,
    ) -> String {
        let value = match call {
            HostImportCall::Bindings => self.host_imports.descriptors().map_or_else(
                |error| serde_json::json!({ "runtime": json_runtime_failure(&error) }),
                |bindings| serde_json::json!({ "ok": bindings }),
            ),
            HostImportCall::Invoke {
                binding_id,
                operation,
                payload,
            } => match parse_host_payload(&payload) {
                Ok(payload) => json_host_invocation_envelope(
                    self.host_imports
                        .invoke(binding_id, operation, payload, context)
                        .await,
                ),
                Err(error) => serde_json::json!({ "runtime": json_runtime_failure(&error) }),
            },
            HostImportCall::StreamOpen {
                binding_id,
                operation,
                payload,
            } => match parse_host_payload(&payload) {
                Ok(payload) => match self
                    .host_imports
                    .clone()
                    .open_stream(binding_id, operation, payload, context)
                    .await
                {
                    Ok(Ok(stream_id)) => serde_json::json!({ "ok": stream_id }),
                    Ok(Err(error)) => serde_json::json!({ "error": error }),
                    Err(error) => serde_json::json!({ "runtime": json_runtime_failure(&error) }),
                },
                Err(error) => serde_json::json!({ "runtime": json_runtime_failure(&error) }),
            },
            HostImportCall::StreamSend { stream_id, payload } => match parse_host_payload(&payload)
            {
                Ok(payload) => match self.host_imports.send_stream(stream_id, payload).await {
                    Ok(()) => serde_json::json!({ "ok": null }),
                    Err(error) => serde_json::json!({ "runtime": json_runtime_failure(&error) }),
                },
                Err(error) => serde_json::json!({ "runtime": json_runtime_failure(&error) }),
            },
            HostImportCall::StreamReceive { stream_id } => {
                match self.host_imports.clone().receive_stream(stream_id).await {
                    Ok(JsonStreamItem::Message(value)) => serde_json::json!({
                        "ok": JsonStreamFrame::Message(value),
                    }),
                    Ok(JsonStreamItem::PeerHalfClosed) => serde_json::json!({
                        "ok": JsonStreamFrame::PeerHalfClosed,
                    }),
                    Ok(JsonStreamItem::Terminal(Ok(()))) => serde_json::json!({
                        "ok": JsonStreamFrame::TerminalSuccess,
                    }),
                    Ok(JsonStreamItem::Terminal(Err(error))) => serde_json::json!({
                        "ok": JsonStreamFrame::TerminalError(error),
                    }),
                    Err(error) => serde_json::json!({ "runtime": json_runtime_failure(&error) }),
                }
            }
            HostImportCall::StreamCloseSend { stream_id } => {
                match self.host_imports.close_stream_send(stream_id).await {
                    Ok(()) => serde_json::json!({ "ok": null }),
                    Err(error) => serde_json::json!({ "runtime": json_runtime_failure(&error) }),
                }
            }
            HostImportCall::StreamCancel { stream_id } => {
                match self.host_imports.cancel_stream(stream_id) {
                    Ok(()) => serde_json::json!({ "ok": null }),
                    Err(error) => serde_json::json!({ "runtime": json_runtime_failure(&error) }),
                }
            }
        };
        serde_json::to_string(&value).expect("host import result is JSON")
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

impl JsonStreamTransport for WasmGeneration {
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
                JsonInvocationOutcome::Success(serde_json::Value::Number(id)) => {
                    let stream_id = id.as_u64().ok_or(RuntimeFailure::ProtocolViolation {
                        capability: EXECUTION_CLASS,
                    })?;
                    Ok(Ok(Rc::new(WasmStreamSession {
                        generation: self,
                        stream_id,
                        context,
                        cancelled: std::cell::Cell::new(false),
                        finished: std::cell::Cell::new(false),
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
struct WasmStreamSession {
    generation: Rc<WasmGeneration>,
    stream_id: u64,
    context: InvocationContext,
    cancelled: std::cell::Cell<bool>,
    finished: std::cell::Cell<bool>,
}

impl WasmStreamSession {
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

impl JsonStreamSessionTransport for WasmStreamSession {
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
                JsonInvocationOutcome::Success(serde_json::Value::Null) => Ok(()),
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
                JsonInvocationOutcome::Success(serde_json::Value::Null) => Ok(()),
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
        let (imports, _import_receiver) = futures_mpsc::channel(1);
        let _ = self
            .generation
            .commands
            .try_send(WorkerCommand::Call(GuestCommand {
                call: GuestCall::StreamCancel {
                    stream_id: self.stream_id,
                },
                abandoned,
                imports,
                outcome,
            }));
    }
}

impl Drop for WasmStreamSession {
    fn drop(&mut self) {
        self.finish();
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
    imports: Option<futures_mpsc::Sender<HostImportCommand>>,
    deadline: mpsc::Sender<DeadlineCommand>,
    max_turn: std::time::Duration,
}

impl host_imports_abi::PluginImports for HostState {
    fn host_bindings(&mut self) -> String {
        call_wasm_host(self, HostImportCall::Bindings)
    }

    fn host_invoke(&mut self, binding_id: u32, operation: String, request_json: String) -> String {
        call_wasm_host(
            self,
            HostImportCall::Invoke {
                binding_id,
                operation,
                payload: request_json,
            },
        )
    }

    fn host_stream_open(
        &mut self,
        binding_id: u32,
        operation: String,
        request_json: String,
    ) -> String {
        call_wasm_host(
            self,
            HostImportCall::StreamOpen {
                binding_id,
                operation,
                payload: request_json,
            },
        )
    }

    fn host_stream_send(&mut self, stream_id: u64, message_json: String) -> String {
        call_wasm_host(
            self,
            HostImportCall::StreamSend {
                stream_id,
                payload: message_json,
            },
        )
    }

    fn host_stream_receive(&mut self, stream_id: u64) -> String {
        call_wasm_host(self, HostImportCall::StreamReceive { stream_id })
    }

    fn host_stream_close_send(&mut self, stream_id: u64) -> String {
        call_wasm_host(self, HostImportCall::StreamCloseSend { stream_id })
    }

    fn host_stream_cancel(&mut self, stream_id: u64) -> String {
        call_wasm_host(self, HostImportCall::StreamCancel { stream_id })
    }
}

fn call_wasm_host(state: &mut HostState, call: HostImportCall) -> String {
    let _ = state.deadline.send(DeadlineCommand::Disarm);
    let (response, receiver) = mpsc::sync_channel(1);
    let result = state
        .imports
        .as_mut()
        .ok_or(())
        .and_then(|sender| {
            sender
                .try_send(HostImportCommand { call, response })
                .map_err(|_| ())
        })
        .and_then(|()| receiver.recv().map_err(|_| ()))
        .unwrap_or_else(|()| {
            serde_json::json!({
                "runtime": { "kind": "admission_closed" }
            })
            .to_string()
        });
    let _ = state.deadline.send(DeadlineCommand::Arm(state.max_turn));
    result
}

#[derive(Clone, Copy)]
struct WasmWorkerInputs<'a> {
    engine: &'a Engine,
    component: &'a Component,
    linker: &'a Linker<HostState>,
    instance: &'a PluginInstancePlan,
    limits: &'a WasmComponentLimits,
}

enum WasmBindings {
    Request(request_abi::Plugin),
    Interactions(interactions_abi::Plugin),
    HostImports(host_imports_abi::Plugin),
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
    let (deadline_tx, deadline_rx) = mpsc::channel();
    let deadline_engine = engine.clone();
    let deadline_worker = thread::Builder::new()
        .name("lenso-wasm-deadline".to_owned())
        .spawn(move || run_deadline_worker(&deadline_engine, &deadline_rx))
        .map_err(|error| error.to_string())?;
    let mut store = Store::new(
        engine,
        HostState {
            limits: store_limits,
            imports: None,
            deadline: deadline_tx.clone(),
            max_turn: limits.max_turn,
        },
    );
    store.limiter(|state| &mut state.limits);
    store
        .set_fuel(limits.fuel_per_invocation)
        .map_err(|error| error.to_string())?;
    store.set_epoch_deadline(1);
    store.epoch_deadline_trap();
    deadline_tx
        .send(DeadlineCommand::Arm(limits.max_turn))
        .map_err(|error| error.to_string())?;
    let requires_stream = instance
        .provided_capabilities()
        .iter()
        .any(|descriptor| !descriptor.stream_operations().is_empty());
    let bindings = if !instance.required_capabilities().is_empty() {
        WasmBindings::HostImports(
            host_imports_abi::Plugin::instantiate(&mut store, component, linker)
                .map_err(|error| bounded(error.to_string()))?,
        )
    } else if requires_stream {
        WasmBindings::Interactions(
            interactions_abi::Plugin::instantiate(&mut store, component, linker)
                .map_err(|error| bounded(error.to_string()))?,
        )
    } else {
        WasmBindings::Request(
            request_abi::Plugin::instantiate(&mut store, component, linker)
                .map_err(|error| bounded(error.to_string()))?,
        )
    };
    let descriptor = match &bindings {
        WasmBindings::Request(bindings) => bindings.call_describe(&mut store),
        WasmBindings::Interactions(bindings) => bindings.call_describe(&mut store),
        WasmBindings::HostImports(bindings) => bindings.call_describe(&mut store),
    }
    .map_err(|error| bounded(format!("Wasm Component describe trapped: {error}")))?;
    if descriptor.len() > limits.max_result_bytes {
        return Err("Wasm Component descriptor exceeds max_result_bytes".to_owned());
    }
    validate_json_plugin_descriptor(instance, &descriptor)
        .map_err(|error| bounded(format!("Wasm Component descriptor mismatch: {error:?}")))?;
    deadline_tx
        .send(DeadlineCommand::Disarm)
        .map_err(|error| error.to_string())?;
    ready.send(Ok(())).map_err(|error| error.to_string())?;
    let worker_result = (|| {
        while let Ok(command) = receiver.recv() {
            match command {
                WorkerCommand::Shutdown => return Ok(()),
                WorkerCommand::Call(command) => {
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
                    store.data_mut().imports = Some(command.imports);
                    let outcome = call_wasm_guest(
                        &bindings,
                        &mut store,
                        &command.call,
                        limits.max_result_bytes,
                    );
                    store.data_mut().imports = None;
                    deadline_tx
                        .send(DeadlineCommand::Disarm)
                        .map_err(|error| error.to_string())?;
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

#[allow(clippy::too_many_lines)]
fn call_wasm_guest(
    bindings: &WasmBindings,
    store: &mut Store<HostState>,
    call: &GuestCall,
    max_result_bytes: usize,
) -> Result<JsonInvocationOutcome, String> {
    let interactions = match bindings {
        WasmBindings::Interactions(bindings) => Some(bindings),
        WasmBindings::Request(_) | WasmBindings::HostImports(_) => None,
    };
    match (bindings, call) {
        (
            WasmBindings::Request(bindings),
            GuestCall::Invoke {
                capability,
                operation,
                payload,
            },
        ) => decode_wasm_json_result(
            bindings.call_invoke(store, capability, operation, payload),
            max_result_bytes,
        ),
        (
            WasmBindings::HostImports(bindings),
            GuestCall::Invoke {
                capability,
                operation,
                payload,
            },
        ) => decode_wasm_json_result(
            bindings.call_invoke(store, capability, operation, payload),
            max_result_bytes,
        ),
        (
            WasmBindings::Interactions(bindings),
            GuestCall::Invoke {
                capability,
                operation,
                payload,
            },
        ) => decode_wasm_json_result(
            bindings.call_invoke(store, capability, operation, payload),
            max_result_bytes,
        ),
        (
            WasmBindings::HostImports(bindings),
            GuestCall::StreamOpen {
                capability,
                operation,
                payload,
            },
        ) => {
            let result = bindings
                .call_stream_open(store, capability, operation, payload)
                .map_err(|error| format!("Wasm Component trapped: {error}"))?;
            match result {
                Ok(stream_id) => Ok(JsonInvocationOutcome::Success(stream_id.into())),
                Err(encoded) => parse_bounded_json(&encoded, max_result_bytes)
                    .map(JsonInvocationOutcome::DomainError),
            }
        }
        (WasmBindings::HostImports(bindings), GuestCall::StreamSend { stream_id, payload }) => {
            bindings
                .call_stream_send(store, *stream_id, payload)
                .map_err(|error| format!("Wasm Component trapped: {error}"))?
                .map_err(|detail| {
                    bounded(format!("Wasm Component stream-send failed: {detail}"))
                })?;
            Ok(JsonInvocationOutcome::Success(serde_json::Value::Null))
        }
        (WasmBindings::HostImports(bindings), GuestCall::StreamReceive { stream_id }) => {
            let encoded = bindings
                .call_stream_receive(store, *stream_id)
                .map_err(|error| format!("Wasm Component trapped: {error}"))?
                .map_err(|detail| {
                    bounded(format!("Wasm Component stream-receive failed: {detail}"))
                })?;
            parse_bounded_json(&encoded, max_result_bytes).map(JsonInvocationOutcome::Success)
        }
        (WasmBindings::HostImports(bindings), GuestCall::StreamCloseSend { stream_id }) => {
            bindings
                .call_stream_close_send(store, *stream_id)
                .map_err(|error| format!("Wasm Component trapped: {error}"))?
                .map_err(|detail| {
                    bounded(format!("Wasm Component stream-close-send failed: {detail}"))
                })?;
            Ok(JsonInvocationOutcome::Success(serde_json::Value::Null))
        }
        (WasmBindings::HostImports(bindings), GuestCall::StreamCancel { stream_id }) => {
            bindings
                .call_stream_cancel(store, *stream_id)
                .map_err(|error| format!("Wasm Component trapped: {error}"))?;
            Ok(JsonInvocationOutcome::Success(serde_json::Value::Null))
        }
        (
            _,
            GuestCall::StreamOpen {
                capability,
                operation,
                payload,
            },
        ) => {
            let result = interactions
                .ok_or_else(|| "request-only Component received stream-open".to_owned())?
                .call_stream_open(store, capability, operation, payload)
                .map_err(|error| format!("Wasm Component trapped: {error}"))?;
            match result {
                Ok(stream_id) => Ok(JsonInvocationOutcome::Success(stream_id.into())),
                Err(encoded) => parse_bounded_json(&encoded, max_result_bytes)
                    .map(JsonInvocationOutcome::DomainError),
            }
        }
        (_, GuestCall::StreamSend { stream_id, payload }) => {
            interactions
                .ok_or_else(|| "request-only Component received stream-send".to_owned())?
                .call_stream_send(store, *stream_id, payload)
                .map_err(|error| format!("Wasm Component trapped: {error}"))?
                .map_err(|detail| {
                    bounded(format!("Wasm Component stream-send failed: {detail}"))
                })?;
            Ok(JsonInvocationOutcome::Success(serde_json::Value::Null))
        }
        (_, GuestCall::StreamReceive { stream_id }) => {
            let encoded = interactions
                .ok_or_else(|| "request-only Component received stream-receive".to_owned())?
                .call_stream_receive(store, *stream_id)
                .map_err(|error| format!("Wasm Component trapped: {error}"))?
                .map_err(|detail| {
                    bounded(format!("Wasm Component stream-receive failed: {detail}"))
                })?;
            parse_bounded_json(&encoded, max_result_bytes).map(JsonInvocationOutcome::Success)
        }
        (_, GuestCall::StreamCloseSend { stream_id }) => {
            interactions
                .ok_or_else(|| "request-only Component received stream-close-send".to_owned())?
                .call_stream_close_send(store, *stream_id)
                .map_err(|error| format!("Wasm Component trapped: {error}"))?
                .map_err(|detail| {
                    bounded(format!("Wasm Component stream-close-send failed: {detail}"))
                })?;
            Ok(JsonInvocationOutcome::Success(serde_json::Value::Null))
        }
        (_, GuestCall::StreamCancel { stream_id }) => {
            interactions
                .ok_or_else(|| "request-only Component received stream-cancel".to_owned())?
                .call_stream_cancel(store, *stream_id)
                .map_err(|error| format!("Wasm Component trapped: {error}"))?;
            Ok(JsonInvocationOutcome::Success(serde_json::Value::Null))
        }
    }
}

fn decode_wasm_json_result(
    result: wasmtime::Result<Result<String, String>>,
    max_result_bytes: usize,
) -> Result<JsonInvocationOutcome, String> {
    match result.map_err(|error| format!("Wasm Component trapped: {error}"))? {
        Ok(encoded) => {
            parse_bounded_json(&encoded, max_result_bytes).map(JsonInvocationOutcome::Success)
        }
        Err(encoded) => {
            parse_bounded_json(&encoded, max_result_bytes).map(JsonInvocationOutcome::DomainError)
        }
    }
}

fn parse_bounded_json(encoded: &str, max_result_bytes: usize) -> Result<serde_json::Value, String> {
    if encoded.len() > max_result_bytes {
        return Err("Component result exceeds max_result_bytes".to_owned());
    }
    serde_json::from_str(encoded).map_err(|error| format!("invalid Component result JSON: {error}"))
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

impl PluginLifecycle for WasmLifecycle {
    fn activate(&self, context: lenso_kernel::ActivateContext) -> lenso_kernel::PluginFuture {
        let result = self
            .generation
            .host_imports
            .activate(context.dependencies());
        Box::pin(futures::future::ready(result))
    }

    fn deactivate(&self, _context: lenso_kernel::DeactivateContext) -> lenso_kernel::PluginFuture {
        self.generation.host_imports.deactivate();
        self.generation.stop();
        Box::pin(futures::future::ready(Ok(())))
    }
}

fn parse_host_payload(encoded: &str) -> Result<serde_json::Value, RuntimeFailure> {
    serde_json::from_str(encoded).map_err(|_| RuntimeFailure::ProtocolViolation {
        capability: JSON_HOST_IMPORTS_ABI_V2,
    })
}

fn wasm_failure(error: impl std::fmt::Display) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: bounded(format!("Wasm Component generation failure: {error}")),
    }
}

fn plugin_failure<T>(detail: impl Into<String>) -> Result<T, RuntimeFailure> {
    Err(RuntimeFailure::PluginFailure {
        detail: bounded(detail.into()),
    })
}

fn invalid<T>(detail: String) -> Result<T, RuntimeFailure> {
    Err(RuntimeFailure::InvalidResolvedPlan { detail })
}

fn bounded(mut detail: String) -> String {
    const MAX_DETAIL: usize = 1024;
    if detail.len() > MAX_DETAIL {
        let mut boundary = MAX_DETAIL;
        while !detail.is_char_boundary(boundary) {
            boundary -= 1;
        }
        detail.truncate(boundary);
    }
    detail
}

#[cfg(test)]
mod tests {
    use super::bounded;

    #[test]
    fn bounded_failure_preserves_utf8() {
        let detail = bounded("界".repeat(400));

        assert_eq!(detail.len(), 1023);
        assert_eq!(detail.chars().count(), 341);
    }
}
