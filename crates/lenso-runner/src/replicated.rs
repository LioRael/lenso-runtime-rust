use std::{
    any::{Any, TypeId},
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    fmt,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
    sync::{Arc, mpsc as std_mpsc},
    thread,
    time::{Duration, Instant},
};

use cpu_time::ThreadTime;
use futures::{channel::oneshot, future::Either};
use lenso_app_plan::{ExecutionLaneId, ResolvedAppPlan};
use lenso_kernel::{
    CancellationToken, EventCapability, ExecutionAdapterCatalog, NativeApp, NativeEventHandle,
    NativeRequestHandle, NativeStream, NativeStreamHandle, RequestCapability, RuntimeDiagnostics,
    RuntimeFailure, ShutdownOutcome, StreamCapability,
};
use tokio::sync::{mpsc, watch};

use crate::TokioDriver;

mod admission;
mod diagnostics;
mod error;
mod interaction_transfer;
mod projection;
mod terminal;
mod transfer;

pub use diagnostics::LaneDiagnosticsSnapshot;
use diagnostics::{LaneDiagnosticsState, LaneInvocationProbe};
pub use error::ReplicatedRunnerError;
use interaction_transfer::CrossLaneInteractionCatalog;
use projection::{LaneProxyAdapter, project_lane};
use terminal::ReplicatedTerminalState;
pub use transfer::CrossLaneRequestCatalog;

const LANE_PROXY_EXECUTION_CLASS: &str = "lenso.native-lane-proxy@1";

/// Placement-independent controls applied by the provider lane's Runtime Driver.
#[derive(Clone, Debug, Default)]
pub struct LaneInvocationOptions {
    timeout: Option<Duration>,
    cancellation: Option<LaneCancellationToken>,
}

impl LaneInvocationOptions {
    /// Creates an invocation without a deadline.
    pub const fn new() -> Self {
        Self {
            timeout: None,
            cancellation: None,
        }
    }

    /// Applies a provider-Driver-relative deadline.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Propagates one caller-owned cooperative cancellation signal.
    #[must_use]
    pub fn with_cancellation(mut self, cancellation: LaneCancellationToken) -> Self {
        self.cancellation = Some(cancellation);
        self
    }
}

/// A thread-safe caller signal translated to a lane-local Kernel cancellation token.
#[derive(Clone, Debug)]
pub struct LaneCancellationToken {
    cancelled: watch::Sender<bool>,
}

impl Default for LaneCancellationToken {
    fn default() -> Self {
        let (cancelled, _) = watch::channel(false);
        Self { cancelled }
    }
}

impl LaneCancellationToken {
    /// Creates a signal that has not been cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cooperative cancellation of attached invocations.
    pub fn cancel(&self) {
        self.cancelled.send_replace(true);
    }

    /// Returns whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        *self.cancelled.borrow()
    }

    async fn cancelled(&self) {
        let mut cancelled = self.cancelled.subscribe();
        loop {
            if *cancelled.borrow_and_update() {
                return;
            }
            if cancelled.changed().await.is_err() {
                return;
            }
        }
    }
}

type LaneTask = Box<dyn FnOnce(LaneRuntime) + Send + 'static>;
type LaneSender = mpsc::Sender<LaneTask>;
type LaneRoute = mpsc::WeakSender<LaneTask>;
type CrossLaneDiagnostics = (Arc<LaneDiagnosticsState>, ExecutionLaneId, Arc<str>);
type RequestRouteIndex = BTreeMap<String, BTreeMap<String, PlannedRequestRoute>>;

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlannedRequestRoute {
    caller_instance: Arc<str>,
    provider_instance: Arc<str>,
    consumer_lane: ExecutionLaneId,
    provider_lane: ExecutionLaneId,
    providers: usize,
}

struct LaneShutdown {
    timeout: Duration,
    completed: oneshot::Sender<ShutdownOutcome>,
}

struct LaneHandle {
    id: ExecutionLaneId,
    commands: LaneSender,
    shutdown: oneshot::Sender<LaneShutdown>,
    thread: thread::JoinHandle<()>,
}

type TypedRequestHandles = HashMap<String, Box<dyn Any>>;
type TypedStreamSessions = HashMap<(TypeId, u64), Box<dyn Any>>;

#[derive(Clone)]
struct LaneRuntime {
    app: NativeApp,
    request_handles: Rc<RefCell<HashMap<TypeId, TypedRequestHandles>>>,
    stream_sessions: Rc<RefCell<TypedStreamSessions>>,
}

impl LaneRuntime {
    fn new(app: NativeApp) -> Self {
        Self {
            app,
            request_handles: Rc::new(RefCell::new(HashMap::new())),
            stream_sessions: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    fn request_handle<C: RequestCapability>(
        &self,
        caller_instance: &str,
    ) -> Result<Rc<NativeRequestHandle<C>>, RuntimeFailure> {
        let capability = TypeId::of::<C>();
        if let Some(handle) = self
            .request_handles
            .borrow()
            .get(&capability)
            .and_then(|handles| handles.get(caller_instance))
            .and_then(|handle| handle.downcast_ref::<Rc<NativeRequestHandle<C>>>())
        {
            return Ok(handle.clone());
        }
        let handle = Rc::new(self.app.handle::<C>(caller_instance)?);
        self.request_handles
            .borrow_mut()
            .entry(capability)
            .or_default()
            .insert(caller_instance.to_owned(), Box::new(handle.clone()));
        Ok(handle)
    }

    fn stream_handle<C: StreamCapability>(
        &self,
        caller_instance: &str,
        provider_instance: &str,
    ) -> Result<NativeStreamHandle<C>, RuntimeFailure> {
        let dependencies = self.app.dependencies(caller_instance)?;
        dependencies
            .bindings()
            .iter()
            .find(|binding| {
                binding.capability_id() == C::ID && binding.provider_instance() == provider_instance
            })
            .and_then(lenso_kernel::PluginDependency::stream_handle)
            .ok_or(RuntimeFailure::Unavailable { capability: C::ID })?
            .typed::<C>()
    }

    fn event_handle<C: EventCapability>(
        &self,
        caller_instance: &str,
        provider_instance: &str,
    ) -> Result<NativeEventHandle<C>, RuntimeFailure> {
        let dependencies = self.app.dependencies(caller_instance)?;
        dependencies
            .bindings()
            .iter()
            .find(|binding| {
                binding.capability_id() == C::ID && binding.provider_instance() == provider_instance
            })
            .and_then(lenso_kernel::PluginDependency::event_handle)
            .ok_or(RuntimeFailure::Unavailable { capability: C::ID })?
            .typed::<C>()
    }

    fn insert_stream<C: StreamCapability>(&self, session_id: u64, stream: NativeStream<C>) {
        self.stream_sessions
            .borrow_mut()
            .insert((TypeId::of::<C>(), session_id), Box::new(Rc::new(stream)));
    }

    fn stream<C: StreamCapability>(
        &self,
        session_id: u64,
    ) -> Result<Rc<NativeStream<C>>, RuntimeFailure> {
        self.stream_sessions
            .borrow()
            .get(&(TypeId::of::<C>(), session_id))
            .and_then(|stream| stream.downcast_ref::<Rc<NativeStream<C>>>())
            .cloned()
            .ok_or(RuntimeFailure::Unavailable { capability: C::ID })
    }

    fn remove_stream<C: StreamCapability>(&self, session_id: u64) {
        self.stream_sessions
            .borrow_mut()
            .remove(&(TypeId::of::<C>(), session_id));
    }
}

/// Generated native values registered for zero-serialization transfer between Kernel lanes.
#[derive(Clone, Debug, Default)]
pub struct CrossLaneTransferCatalog {
    requests: CrossLaneRequestCatalog,
    interactions: CrossLaneInteractionCatalog,
}

impl CrossLaneTransferCatalog {
    /// Creates an empty catalog for a Plan whose bindings remain on one lane.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one generated request Capability whose values are `Send`.
    #[must_use]
    pub fn with_request<C>(mut self, operations: &'static [&'static str]) -> Self
    where
        C: RequestCapability,
        C::Request: Send,
        C::Response: Send,
        C::DomainError: Send,
    {
        self.requests = self.requests.with_request::<C>(operations);
        self
    }

    /// Registers one generated stream Capability whose values are `Send`.
    #[must_use]
    pub fn with_stream<C>(mut self, operations: &'static [&'static str]) -> Self
    where
        C: StreamCapability,
        C::OpenRequest: Send,
        C::Message: Send,
        C::DomainError: Send,
    {
        self.interactions = self.interactions.with_stream::<C>(operations);
        self
    }

    /// Registers one generated ephemeral Event Capability whose values are `Send`.
    #[must_use]
    pub fn with_event<C>(mut self, operations: &'static [&'static str]) -> Self
    where
        C: EventCapability,
        C::Event: Send,
    {
        self.interactions = self.interactions.with_event::<C>(operations);
        self
    }

    fn validate_plan(&self, plan: &ResolvedAppPlan) -> Result<(), ReplicatedRunnerError> {
        self.requests.validate_plan(plan)?;
        self.interactions.validate_plan(plan)
    }
}

impl From<CrossLaneRequestCatalog> for CrossLaneTransferCatalog {
    fn from(requests: CrossLaneRequestCatalog) -> Self {
        Self {
            requests,
            interactions: CrossLaneInteractionCatalog::default(),
        }
    }
}

/// One native App executed as a fixed set of Plan-declared single-owner Kernel lanes.
pub struct ReplicatedNativeApp {
    request_routes: Arc<RequestRouteIndex>,
    lanes: BTreeMap<ExecutionLaneId, LaneHandle>,
    diagnostics: Arc<LaneDiagnosticsState>,
    terminal: Arc<ReplicatedTerminalState>,
    epoch: Instant,
}

#[derive(Clone)]
struct ReplicatedLaneRoute {
    id: ExecutionLaneId,
    commands: LaneSender,
}

/// Cloneable invocation target for one complete replicated App Generation.
#[derive(Clone)]
pub struct ReplicatedAppRoute {
    request_routes: Arc<RequestRouteIndex>,
    lanes: BTreeMap<ExecutionLaneId, ReplicatedLaneRoute>,
    diagnostics: Arc<LaneDiagnosticsState>,
    terminal: Arc<ReplicatedTerminalState>,
    epoch: Instant,
}

impl fmt::Debug for ReplicatedAppRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReplicatedAppRoute")
            .field("lanes", &self.lanes.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ReplicatedNativeApp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReplicatedNativeApp")
            .field("lanes", &self.lanes.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl ReplicatedNativeApp {
    /// Starts one unmodified Kernel replica per declared Execution Lane.
    pub fn start<F>(plan: ResolvedAppPlan, adapters: F) -> Result<Self, ReplicatedRunnerError>
    where
        F: Fn(&ExecutionLaneId) -> ExecutionAdapterCatalog + Send + Sync + 'static,
    {
        Self::start_fallible(plan, move |lane| Ok(adapters(lane)))
    }

    /// Starts every declared lane with fallible lane-local Adapter assembly.
    pub fn start_fallible<F>(
        plan: ResolvedAppPlan,
        adapters: F,
    ) -> Result<Self, ReplicatedRunnerError>
    where
        F: Fn(&ExecutionLaneId) -> Result<ExecutionAdapterCatalog, String> + Send + Sync + 'static,
    {
        Self::start_with_fallible_transfer_catalog(
            plan,
            adapters,
            CrossLaneTransferCatalog::new(),
            None,
        )
    }

    /// Starts every lane and fails closed unless the complete Lane Set is Ready in time.
    pub fn start_fallible_with_timeout<F>(
        plan: ResolvedAppPlan,
        adapters: F,
        ready_timeout: Duration,
    ) -> Result<Self, ReplicatedRunnerError>
    where
        F: Fn(&ExecutionLaneId) -> Result<ExecutionAdapterCatalog, String> + Send + Sync + 'static,
    {
        Self::start_with_fallible_transfer_catalog(
            plan,
            adapters,
            CrossLaneTransferCatalog::new(),
            Some(ready_timeout),
        )
    }

    /// Starts replicated lanes with generated request types allowed to cross them.
    #[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
    pub fn start_with_transfers<F>(
        plan: ResolvedAppPlan,
        adapters: F,
        transfers: CrossLaneRequestCatalog,
    ) -> Result<Self, ReplicatedRunnerError>
    where
        F: Fn(&ExecutionLaneId) -> ExecutionAdapterCatalog + Send + Sync + 'static,
    {
        Self::start_with_transfer_catalog(plan, adapters, transfers.into())
    }

    /// Starts replicated lanes with generated values allowed to cross them without serialization.
    #[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
    pub fn start_with_transfer_catalog<F>(
        plan: ResolvedAppPlan,
        adapters: F,
        transfers: CrossLaneTransferCatalog,
    ) -> Result<Self, ReplicatedRunnerError>
    where
        F: Fn(&ExecutionLaneId) -> ExecutionAdapterCatalog + Send + Sync + 'static,
    {
        Self::start_with_fallible_transfer_catalog(
            plan,
            move |lane| Ok(adapters(lane)),
            transfers,
            None,
        )
    }

    /// Starts a complete Lane Set with fallible Adapters, transfers, and one Ready deadline.
    #[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
    pub fn start_with_fallible_transfer_catalog<F>(
        plan: ResolvedAppPlan,
        adapters: F,
        transfers: CrossLaneTransferCatalog,
        ready_timeout: Option<Duration>,
    ) -> Result<Self, ReplicatedRunnerError>
    where
        F: Fn(&ExecutionLaneId) -> Result<ExecutionAdapterCatalog, String> + Send + Sync + 'static,
    {
        plan.validate()
            .map_err(|error| ReplicatedRunnerError::InvalidPlan {
                detail: error.to_string(),
            })?;
        transfers.validate_plan(&plan)?;
        let request_routes = Arc::new(index_request_routes(&plan));
        let plan = Arc::new(plan);
        let adapters = Arc::new(adapters);
        let diagnostics = Arc::new(LaneDiagnosticsState::new(Arc::clone(&plan)));
        let terminal = Arc::new(ReplicatedTerminalState::default());
        let epoch = Instant::now();
        let mut receivers = BTreeMap::new();
        let senders = plan
            .execution_lanes()
            .iter()
            .map(|lane| {
                let (sender, receiver) = mpsc::channel(64);
                receivers.insert(lane.id().clone(), receiver);
                (lane.id().clone(), sender)
            })
            .collect::<BTreeMap<_, _>>();
        let routes = Arc::new(
            senders
                .iter()
                .map(|(lane, sender)| (lane.clone(), sender.downgrade()))
                .collect::<BTreeMap<_, _>>(),
        );
        let projected = plan
            .execution_lanes()
            .iter()
            .map(|lane| {
                project_lane(&plan, lane.id()).map(|projected| (lane.id().clone(), projected))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut lanes = BTreeMap::new();
        let mut startups = Vec::new();

        for (lane_id, lane_plan) in projected {
            let commands = senders
                .get(&lane_id)
                .expect("every declared lane has a command route")
                .clone();
            let receiver = receivers
                .remove(&lane_id)
                .expect("every declared lane has one command receiver");
            let (shutdown, shutdown_request) = oneshot::channel();
            let (started, startup) = std_mpsc::sync_channel(1);
            let lane_adapters = Arc::clone(&adapters);
            let lane_diagnostics = Arc::clone(&diagnostics);
            let lane_terminal = Arc::clone(&terminal);
            let proxy_adapter = LaneProxyAdapter::new(
                Arc::clone(&plan),
                transfers.clone(),
                Arc::clone(&routes),
                epoch,
            );
            let thread_lane = lane_id.clone();
            let lane_thread = match thread::Builder::new()
                .name(format!("lenso-lane-{}", lane_id.as_str()))
                .spawn(move || {
                    let reported_lane = thread_lane.clone();
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        run_lane(
                            thread_lane,
                            lane_plan,
                            receiver,
                            shutdown_request,
                            started,
                            lane_adapters,
                            proxy_adapter,
                            lane_diagnostics,
                            Arc::clone(&lane_terminal),
                            epoch,
                        );
                    }));
                    if result.is_err() {
                        lane_terminal.fail(ReplicatedRunnerError::LanePanicked {
                            lane: reported_lane.to_string(),
                        });
                    }
                }) {
                Ok(thread) => thread,
                Err(error) => {
                    drop(receivers);
                    drop(routes);
                    drop(senders);
                    terminal.begin_shutdown();
                    stop_lanes(lanes);
                    return Err(ReplicatedRunnerError::LaneStartup {
                        lane: lane_id.to_string(),
                        detail: error.to_string(),
                    });
                }
            };
            startups.push((lane_id.clone(), startup));
            lanes.insert(
                lane_id.clone(),
                LaneHandle {
                    id: lane_id,
                    commands,
                    shutdown,
                    thread: lane_thread,
                },
            );
        }

        let ready_deadline = ready_timeout.and_then(|timeout| Instant::now().checked_add(timeout));
        for (lane, startup) in startups {
            let startup = if let Some(deadline) = ready_deadline {
                startup.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            } else {
                startup
                    .recv()
                    .map_err(|_| std_mpsc::RecvTimeoutError::Disconnected)
            };
            match startup {
                Ok(Ok(())) => {}
                Ok(Err(detail)) => {
                    drop(routes);
                    drop(senders);
                    terminal.begin_shutdown();
                    stop_lanes(lanes);
                    return Err(ReplicatedRunnerError::LaneStartup {
                        lane: lane.to_string(),
                        detail,
                    });
                }
                Err(error) => {
                    drop(routes);
                    drop(senders);
                    let failure = terminal.failure().unwrap_or_else(|| {
                        if error == std_mpsc::RecvTimeoutError::Timeout {
                            ReplicatedRunnerError::LaneStartup {
                                lane: lane.to_string(),
                                detail: "complete App Generation Ready Gate timed out".to_owned(),
                            }
                        } else {
                            ReplicatedRunnerError::LaneUnavailable {
                                lane: lane.to_string(),
                            }
                        }
                    });
                    terminal.begin_shutdown();
                    stop_lanes(lanes);
                    return Err(failure);
                }
            }
        }

        Ok(Self {
            request_routes,
            lanes,
            diagnostics,
            terminal,
            epoch,
        })
    }

    /// Returns the fixed number of Kernel replicas started from the Plan.
    pub fn lane_count(&self) -> usize {
        self.lanes.len()
    }

    /// Returns structural evidence for placement decisions without exposing payloads.
    pub fn diagnostics_snapshot(&self) -> LaneDiagnosticsSnapshot {
        self.diagnostics.snapshot()
    }

    /// Returns whether any Kernel lane reached a terminal failure.
    pub fn is_failed(&self) -> bool {
        self.terminal.is_failed()
    }

    /// Returns the first App-terminal lane failure, when one has occurred.
    pub fn terminal_failure(&self) -> Option<ReplicatedRunnerError> {
        self.terminal.failure()
    }

    /// Waits until one lane makes the replicated App terminal.
    pub async fn wait_for_terminal(&self) -> ReplicatedRunnerError {
        self.terminal.wait().await
    }

    /// Projects a cloneable route which remains pinned by its Generation Lease.
    pub fn route(&self) -> ReplicatedAppRoute {
        ReplicatedAppRoute {
            request_routes: Arc::clone(&self.request_routes),
            lanes: self
                .lanes
                .iter()
                .map(|(lane, handle)| {
                    (
                        lane.clone(),
                        ReplicatedLaneRoute {
                            id: handle.id.clone(),
                            commands: handle.commands.clone(),
                        },
                    )
                })
                .collect(),
            diagnostics: Arc::clone(&self.diagnostics),
            terminal: Arc::clone(&self.terminal),
            epoch: self.epoch,
        }
    }
}

impl ReplicatedAppRoute {
    fn ensure_running(&self) -> Result<(), RuntimeFailure> {
        if let Some(failure) = self.terminal.failure() {
            return Err(RuntimeFailure::Internal {
                detail: failure.to_string(),
            });
        }
        Ok(())
    }

    /// Returns the fixed number of Kernel lanes in this routed Generation.
    pub fn lane_count(&self) -> usize {
        self.lanes.len()
    }

    /// Returns structural evidence for placement decisions without exposing payloads.
    pub fn diagnostics_snapshot(&self) -> LaneDiagnosticsSnapshot {
        self.diagnostics.snapshot()
    }

    /// Returns whether any Kernel lane reached a terminal failure.
    pub fn is_failed(&self) -> bool {
        self.terminal.is_failed()
    }

    /// Returns the first App-terminal lane failure, when one has occurred.
    pub fn terminal_failure(&self) -> Option<ReplicatedRunnerError> {
        self.terminal.failure()
    }

    fn resolve_request_lane<C: RequestCapability>(
        &self,
        caller_instance: &str,
    ) -> Result<(&ReplicatedLaneRoute, Arc<str>, Option<CrossLaneDiagnostics>), RuntimeFailure>
    {
        let route = resolve_planned_request_route(&self.request_routes, caller_instance, C::ID)?;
        let lane =
            self.lanes
                .get(&route.provider_lane)
                .ok_or_else(|| RuntimeFailure::Internal {
                    detail: format!("Execution Lane `{}` is unavailable", route.provider_lane),
                })?;
        let diagnostics = (route.consumer_lane != route.provider_lane).then(|| {
            (
                Arc::clone(&self.diagnostics),
                route.consumer_lane.clone(),
                Arc::clone(&route.provider_instance),
            )
        });
        Ok((lane, Arc::clone(&route.caller_instance), diagnostics))
    }

    /// Invokes one generated request Capability on its Plan-placed provider lane.
    pub async fn invoke<C: RequestCapability>(
        &self,
        caller_instance: &str,
        operation: &str,
        request: C::Request,
    ) -> Result<Result<C::Response, C::DomainError>, RuntimeFailure>
    where
        C::Request: Send,
        C::Response: Send,
        C::DomainError: Send,
    {
        self.invoke_with_options::<C>(
            caller_instance,
            operation,
            request,
            LaneInvocationOptions::new(),
        )
        .await
    }

    /// Invokes one generated request with Driver-relative controls on its provider lane.
    ///
    /// A deadline or cancellation observed before the provider Kernel allocates
    /// an invocation identity reports request ID zero.
    pub async fn invoke_with_options<C: RequestCapability>(
        &self,
        caller_instance: &str,
        operation: &str,
        request: C::Request,
        options: LaneInvocationOptions,
    ) -> Result<Result<C::Response, C::DomainError>, RuntimeFailure>
    where
        C::Request: Send,
        C::Response: Send,
        C::DomainError: Send,
    {
        self.ensure_running()?;
        // An invocation entering through the Runner can start on the resolved provider owner.
        // Calls originating inside a Plugin still use the projected cross-lane transfer endpoint.
        let (lane, caller_instance, cross_lane_diagnostics) =
            self.resolve_request_lane::<C>(caller_instance)?;
        let operation = operation.to_owned();
        let deadline = options
            .timeout
            .map(|timeout| self.epoch.elapsed().saturating_add(timeout));
        let admission_timeout = options.timeout;
        let admission_cancellation = options.cancellation.clone();
        let controlled = deadline.is_some() || admission_cancellation.is_some();
        let (completed, completion) = oneshot::channel();
        let (started, start) = if controlled {
            let (started, start) = oneshot::channel();
            (Some(started), Some(start))
        } else {
            (None, None)
        };
        let task = Box::new(move |lane: LaneRuntime| {
            if let Some((diagnostics, caller_lane, provider_instance)) = cross_lane_diagnostics {
                diagnostics.record_invocation(&caller_lane, &caller_instance, &provider_instance);
            }
            tokio::task::spawn_local(async move {
                let handle = match lane.request_handle::<C>(&caller_instance) {
                    Ok(handle) => handle,
                    Err(error) => {
                        let _ = completed.send(Err(error));
                        return;
                    }
                };
                let cancellation = CancellationToken::new();
                let external_cancellation = options.cancellation;
                if external_cancellation
                    .as_ref()
                    .is_some_and(LaneCancellationToken::is_cancelled)
                {
                    cancellation.cancel();
                }
                let invocation = if deadline.is_some() || external_cancellation.is_some() {
                    let context = lane.app.invocation_context(deadline, cancellation.clone());
                    if let Some(started) = started {
                        let _ = started.send(());
                    }
                    Either::Left(handle.invoke_with_context(&operation, context, request))
                } else {
                    Either::Right(handle.invoke(&operation, request))
                };
                tokio::pin!(invocation);
                let result = if let Some(external_cancellation) = external_cancellation {
                    tokio::select! {
                        result = &mut invocation => result,
                        () = external_cancellation.cancelled() => {
                            cancellation.cancel();
                            invocation.await
                        }
                    }
                } else {
                    invocation.await
                };
                let _ = completed.send(result);
            });
        });
        if let Some(start) = start {
            return admission::dispatch_controlled(
                &lane.id,
                &lane.commands,
                task,
                start,
                completion,
                admission_timeout,
                admission_cancellation,
            )
            .await?;
        }
        lane.commands
            .send(task)
            .await
            .map_err(|_| RuntimeFailure::Internal {
                detail: format!("Execution Lane `{}` is unavailable", lane.id),
            })?;
        completion.await.map_err(|_| RuntimeFailure::Internal {
            detail: format!("Execution Lane `{}` dropped an invocation", lane.id),
        })?
    }
}

impl ReplicatedNativeApp {
    /// Invokes one generated request Capability on its Plan-placed provider lane.
    pub async fn invoke<C: RequestCapability>(
        &self,
        caller_instance: &str,
        operation: &str,
        request: C::Request,
    ) -> Result<Result<C::Response, C::DomainError>, RuntimeFailure>
    where
        C::Request: Send,
        C::Response: Send,
        C::DomainError: Send,
    {
        self.route()
            .invoke::<C>(caller_instance, operation, request)
            .await
    }

    /// Invokes one generated request with Driver-relative controls.
    pub async fn invoke_with_options<C: RequestCapability>(
        &self,
        caller_instance: &str,
        operation: &str,
        request: C::Request,
        options: LaneInvocationOptions,
    ) -> Result<Result<C::Response, C::DomainError>, RuntimeFailure>
    where
        C::Request: Send,
        C::Response: Send,
        C::DomainError: Send,
    {
        self.route()
            .invoke_with_options::<C>(caller_instance, operation, request, options)
            .await
    }

    /// Stops every Kernel replica with the same bounded shutdown timeout.
    pub async fn shutdown(self, timeout: Duration) -> Result<(), ReplicatedRunnerError> {
        self.terminal.begin_shutdown();
        let mut completions = Vec::new();
        let mut threads = Vec::new();
        let mut first_error = self.terminal.failure();
        for (_, lane) in self.lanes {
            let LaneHandle {
                id,
                commands,
                shutdown,
                thread,
            } = lane;
            let (completed, completion) = oneshot::channel();
            if shutdown.send(LaneShutdown { timeout, completed }).is_ok() {
                completions.push((id.clone(), completion));
            } else if first_error.is_none() {
                first_error = Some(ReplicatedRunnerError::LaneUnavailable {
                    lane: id.to_string(),
                });
            }
            drop(commands);
            threads.push((id, thread));
        }

        for (lane, completion) in completions {
            match completion.await {
                Ok(ShutdownOutcome::Clean) => {}
                Ok(outcome) if first_error.is_none() => {
                    first_error = Some(ReplicatedRunnerError::LaneShutdown {
                        lane: lane.to_string(),
                        outcome,
                    });
                }
                Err(_) if first_error.is_none() => {
                    first_error = Some(ReplicatedRunnerError::LaneUnavailable {
                        lane: lane.to_string(),
                    });
                }
                _ => {}
            }
        }
        for (lane, thread) in threads {
            if thread.join().is_err() && first_error.is_none() {
                first_error = Some(ReplicatedRunnerError::LanePanicked {
                    lane: lane.to_string(),
                });
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

fn stop_lanes(lanes: BTreeMap<ExecutionLaneId, LaneHandle>) {
    let mut threads = Vec::new();
    for (_, lane) in lanes {
        let (completed, _) = oneshot::channel();
        let _ = lane.shutdown.send(LaneShutdown {
            timeout: Duration::from_secs(1),
            completed,
        });
        threads.push(lane.thread);
    }
    for thread in threads {
        let _ = thread.join();
    }
}

fn index_request_routes(plan: &ResolvedAppPlan) -> RequestRouteIndex {
    let mut routes = RequestRouteIndex::new();
    for binding in plan.capability_bindings() {
        let consumer = plan
            .plugin_instance(binding.consumer_instance())
            .expect("validated binding consumer should exist");
        let provider = plan
            .plugin_instance(binding.provider_instance())
            .expect("validated binding provider should exist");
        let capabilities = routes
            .entry(binding.consumer_instance().to_owned())
            .or_default();
        let route = capabilities
            .entry(binding.capability_id().to_owned())
            .or_insert_with(|| PlannedRequestRoute {
                caller_instance: Arc::from(binding.consumer_instance()),
                provider_instance: Arc::from(binding.provider_instance()),
                consumer_lane: consumer.execution_lane().clone(),
                provider_lane: provider.execution_lane().clone(),
                providers: 0,
            });
        route.providers += 1;
    }
    routes
}

fn resolve_planned_request_route<'a>(
    routes: &'a RequestRouteIndex,
    caller_instance: &str,
    capability: &'static str,
) -> Result<&'a PlannedRequestRoute, RuntimeFailure> {
    let route = routes
        .get(caller_instance)
        .and_then(|capabilities| capabilities.get(capability))
        .ok_or(RuntimeFailure::Unavailable { capability })?;
    if route.providers != 1 {
        return Err(RuntimeFailure::AmbiguousBinding {
            capability,
            providers: route.providers,
        });
    }
    Ok(route)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn run_lane<F>(
    lane: ExecutionLaneId,
    plan: ResolvedAppPlan,
    mut commands: mpsc::Receiver<LaneTask>,
    mut shutdown: oneshot::Receiver<LaneShutdown>,
    started: std_mpsc::SyncSender<Result<(), String>>,
    adapters: Arc<F>,
    proxy_adapter: LaneProxyAdapter,
    diagnostics: Arc<LaneDiagnosticsState>,
    terminal: Arc<ReplicatedTerminalState>,
    epoch: Instant,
) where
    F: Fn(&ExecutionLaneId) -> Result<ExecutionAdapterCatalog, String> + Send + Sync + 'static,
{
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = started.send(Err(error.to_string()));
            return;
        }
    };
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async move {
        let cpu_started = ThreadTime::now();
        let catalog = match adapters(&lane) {
            Ok(catalog) => match catalog.with_adapter(proxy_adapter) {
                Ok(catalog) => catalog,
                Err(error) => {
                    let _ = started.send(Err(error.to_string()));
                    return;
                }
            },
            Err(detail) => {
                let _ = started.send(Err(detail));
                return;
            }
        };
        let driver = TokioDriver::with_epoch(epoch);
        let runtime_diagnostics = RuntimeDiagnostics::new().with_invocation_probe(Rc::new(
            LaneInvocationProbe::new(Arc::clone(&diagnostics), lane.clone()),
        ));
        let start = lenso_kernel::Kernel::start_with_diagnostics(
            plan,
            driver,
            catalog,
            runtime_diagnostics,
        );
        tokio::pin!(start);
        let app = match tokio::select! {
            result = &mut start => Some(result),
            _ = &mut shutdown => None,
        } {
            Some(Ok(app)) => app,
            Some(Err(error)) => {
                let _ = started.send(Err(format!("{error:?}")));
                return;
            }
            None => {
                let _ = started.send(Err("lane startup cancelled before Ready".to_owned()));
                return;
            }
        };
        let lane_runtime = LaneRuntime::new(app.clone());
        let _ = started.send(Ok(()));
        diagnostics.publish_lane(&lane, &app, cpu_started.elapsed());
        let mut sample_interval = tokio::time::interval(Duration::from_millis(10));
        let terminal_monitor = tokio::task::spawn_local(monitor_lane_failure(
            lane.clone(),
            app.clone(),
            Arc::clone(&terminal),
        ));
        let terminal_failure = terminal.wait();
        tokio::pin!(terminal_failure);

        loop {
            tokio::select! {
                biased;
                shutdown = &mut shutdown => {
                    terminal_monitor.abort();
                    match shutdown {
                        Ok(LaneShutdown { timeout, completed }) => {
                            let outcome = app.shutdown(timeout).await;
                            let _ = completed.send(outcome);
                        }
                        Err(_) => {
                            let _ = app.shutdown(Duration::from_secs(1)).await;
                        }
                    }
                    break;
                }
                _ = &mut terminal_failure => {
                    terminal_monitor.abort();
                    let _ = app.shutdown(Duration::from_secs(1)).await;
                    break;
                },
                command = commands.recv() => if let Some(task) = command {
                    task(lane_runtime.clone());
                } else {
                    terminal_monitor.abort();
                    if !terminal.is_stopping() {
                        terminal.fail(ReplicatedRunnerError::LaneUnavailable {
                            lane: lane.to_string(),
                        });
                    }
                    let _ = app.shutdown(Duration::from_secs(1)).await;
                    break;
                },
                _ = sample_interval.tick() => {
                    diagnostics.publish_lane(&lane, &app, cpu_started.elapsed());
                }
            }
        }
    });
}

async fn monitor_lane_failure(
    lane: ExecutionLaneId,
    app: NativeApp,
    terminal: Arc<ReplicatedTerminalState>,
) {
    let mut failure_interval = tokio::time::interval(Duration::from_millis(10));
    loop {
        failure_interval.tick().await;
        if let Some(error) = app.terminal_failure() {
            terminal.fail(ReplicatedRunnerError::LaneRuntimeFailure {
                lane: lane.to_string(),
                error,
            });
            return;
        }
        if terminal.is_failed() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use lenso_app_plan::{
        AppComposition, CapabilityBinding, CapabilityEndpointPlan, CapabilityRequirementPlan,
        ExecutionLaneId, ExecutionLanePlan, PluginInstancePlan,
    };
    use lenso_kernel::{ExecutionAdapterCatalog, RuntimeFailure};

    use super::{
        PlannedRequestRoute, ReplicatedNativeApp, ReplicatedRunnerError, RequestRouteIndex,
        index_request_routes, resolve_planned_request_route,
    };

    const ROUTE_CAPABILITY: &str = "test.route@1";
    const ROUTE_VERSION: &str = "1.0.0";

    #[test]
    fn request_route_index_preserves_missing_single_and_ambiguous_semantics() {
        let plan = AppComposition::new(
            vec![
                PluginInstancePlan::new("single-consumer", "fixture.consumer").with_requirement(
                    CapabilityRequirementPlan::one(ROUTE_CAPABILITY, ROUTE_VERSION),
                ),
                PluginInstancePlan::new("many-consumer", "fixture.consumer").with_requirement(
                    CapabilityRequirementPlan::many(ROUTE_CAPABILITY, ROUTE_VERSION),
                ),
                PluginInstancePlan::new("provider-a", "fixture.provider").with_capability(
                    CapabilityEndpointPlan::new(ROUTE_CAPABILITY, ROUTE_VERSION, ["route"]),
                ),
                PluginInstancePlan::new("provider-b", "fixture.provider").with_capability(
                    CapabilityEndpointPlan::new(ROUTE_CAPABILITY, ROUTE_VERSION, ["route"]),
                ),
            ],
            vec![
                CapabilityBinding::new(
                    "single-consumer",
                    ROUTE_CAPABILITY,
                    ROUTE_VERSION,
                    "provider-a",
                ),
                CapabilityBinding::new(
                    "many-consumer",
                    ROUTE_CAPABILITY,
                    ROUTE_VERSION,
                    "provider-a",
                ),
                CapabilityBinding::new(
                    "many-consumer",
                    ROUTE_CAPABILITY,
                    ROUTE_VERSION,
                    "provider-b",
                ),
            ],
        )
        .resolve()
        .expect("route fixture should resolve");
        let routes = index_request_routes(&plan);

        assert_eq!(
            resolve_planned_request_route(&routes, "missing", ROUTE_CAPABILITY),
            Err(RuntimeFailure::Unavailable {
                capability: ROUTE_CAPABILITY
            })
        );
        let single = resolve_planned_request_route(&routes, "single-consumer", ROUTE_CAPABILITY)
            .expect("the singular binding should resolve");
        assert_eq!(single.providers, 1);
        assert_eq!(&*single.provider_instance, "provider-a");
        assert_eq!(
            resolve_planned_request_route(&routes, "many-consumer", ROUTE_CAPABILITY),
            Err(RuntimeFailure::AmbiguousBinding {
                capability: ROUTE_CAPABILITY,
                providers: 2,
            })
        );
    }

    #[test]
    fn large_request_route_index_is_self_contained_after_plan_resolution() {
        const BINDINGS: usize = 512;
        let mut instances = Vec::with_capacity(BINDINGS + 1);
        let mut bindings = Vec::with_capacity(BINDINGS);
        instances.push(
            PluginInstancePlan::new("provider", "fixture.provider").with_capability(
                CapabilityEndpointPlan::new(ROUTE_CAPABILITY, ROUTE_VERSION, ["route"]),
            ),
        );
        for index in 0..BINDINGS {
            let consumer = format!("consumer-{index}");
            instances.push(
                PluginInstancePlan::new(&consumer, "fixture.consumer").with_requirement(
                    CapabilityRequirementPlan::one(ROUTE_CAPABILITY, ROUTE_VERSION),
                ),
            );
            bindings.push(CapabilityBinding::new(
                consumer,
                ROUTE_CAPABILITY,
                ROUTE_VERSION,
                "provider",
            ));
        }
        let routes = index_request_routes(
            &AppComposition::new(instances, bindings)
                .resolve()
                .expect("large route fixture should resolve"),
        );

        assert_eq!(routes.len(), BINDINGS);
        for index in 0..BINDINGS {
            let route = resolve_planned_request_route(
                &routes,
                &format!("consumer-{index}"),
                ROUTE_CAPABILITY,
            )
            .expect("every indexed route should remain available");
            assert_eq!(&*route.provider_instance, "provider");
            assert_eq!(route.providers, 1);
        }
    }

    /// Reproducible evidence command:
    /// `cargo test --release -p lenso-runner indexed_route_lookup_benchmark -- --ignored --nocapture`
    #[test]
    #[ignore = "route-index microbenchmark; run explicitly when changing replicated routing"]
    fn indexed_route_lookup_benchmark() {
        const LOOKUPS: usize = 5_000_000;

        fn routes(bindings: usize) -> RequestRouteIndex {
            (0..bindings)
                .map(|index| {
                    (
                        format!("consumer-{index}"),
                        [(
                            ROUTE_CAPABILITY.to_owned(),
                            PlannedRequestRoute {
                                caller_instance: format!("consumer-{index}").into(),
                                provider_instance: "provider".into(),
                                consumer_lane: ExecutionLaneId::new("frontend"),
                                provider_lane: ExecutionLaneId::new("workers"),
                                providers: 1,
                            },
                        )]
                        .into_iter()
                        .collect(),
                    )
                })
                .collect()
        }

        fn nanoseconds_per_lookup(routes: &RequestRouteIndex, caller: &str) -> f64 {
            let started = std::time::Instant::now();
            for _ in 0..LOOKUPS {
                let route = resolve_planned_request_route(routes, caller, ROUTE_CAPABILITY)
                    .expect("indexed benchmark route should resolve");
                std::hint::black_box(route);
            }
            started.elapsed().as_secs_f64() * 1_000_000_000.0
                / f64::from(u32::try_from(LOOKUPS).expect("lookup count fits u32"))
        }

        let small = routes(1);
        let large = routes(8_192);
        let small_ns = nanoseconds_per_lookup(&small, "consumer-0");
        let large_ns = nanoseconds_per_lookup(&large, "consumer-8191");
        println!(
            "{{\"lookups\":{LOOKUPS},\"small_bindings\":1,\"large_bindings\":8192,\"small_ns_per_lookup\":{small_ns:.3},\"large_ns_per_lookup\":{large_ns:.3},\"large_to_small_ratio\":{:.3}}}",
            large_ns / small_ns,
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn one_lane_panic_makes_the_replicated_app_terminal_and_stops_its_peers() {
        let plan = AppComposition::new(Vec::new(), Vec::new())
            .with_execution_lanes(vec![
                ExecutionLanePlan::new("lane-a"),
                ExecutionLanePlan::new("lane-b"),
            ])
            .resolve()
            .expect("the empty two-lane Plan should resolve");
        let app = ReplicatedNativeApp::start(plan, |_| ExecutionAdapterCatalog::new())
            .expect("both empty Kernel lanes should start");
        let peer_commands = app
            .lanes
            .get(&ExecutionLaneId::new("lane-b"))
            .expect("lane-b should exist")
            .commands
            .clone();
        let flooding =
            tokio::spawn(
                async move { while peer_commands.send(Box::new(|_| {})).await.is_ok() {} },
            );
        app.lanes
            .get(&ExecutionLaneId::new("lane-a"))
            .expect("lane-a should exist")
            .commands
            .send(Box::new(|_| panic!("injected lane panic")))
            .await
            .expect("lane-a should accept the injected task");

        let failure = tokio::time::timeout(Duration::from_secs(1), app.wait_for_terminal())
            .await
            .expect("the lane panic should become terminal promptly");
        assert_eq!(
            failure,
            ReplicatedRunnerError::LanePanicked {
                lane: "lane-a".to_owned(),
            }
        );
        assert!(app.is_failed());
        assert_eq!(app.terminal_failure(), Some(failure.clone()));
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), app.shutdown(Duration::from_secs(1)))
                .await
                .expect("a saturated peer lane should still observe terminal failure"),
            Err(failure)
        );
        flooding
            .await
            .expect("the peer command producer should stop when the lane closes");
    }
}
