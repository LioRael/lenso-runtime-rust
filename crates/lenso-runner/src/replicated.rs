use std::{
    any::{Any, TypeId},
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    fmt,
    rc::Rc,
    sync::{Arc, mpsc as std_mpsc},
    thread,
    time::{Duration, Instant},
};

use cpu_time::ThreadTime;
use futures::{channel::oneshot, future::Either};
use lenso_app_plan::{CapabilityBinding, ExecutionLaneId, ResolvedAppPlan};
use lenso_kernel::{
    CancellationToken, ExecutionAdapterCatalog, NativeApp, NativeRequestHandle, RequestCapability,
    RuntimeDiagnostics, RuntimeFailure, ShutdownOutcome,
};
use tokio::sync::{mpsc, watch};

use crate::TokioDriver;

mod diagnostics;
mod projection;
mod transfer;

pub use diagnostics::LaneDiagnosticsSnapshot;
use diagnostics::{LaneDiagnosticsState, LaneInvocationProbe};
use projection::{LaneProxyAdapter, project_lane};
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

#[derive(Clone)]
struct LaneRuntime {
    app: NativeApp,
    request_handles: Rc<RefCell<HashMap<TypeId, TypedRequestHandles>>>,
}

impl LaneRuntime {
    fn new(app: NativeApp) -> Self {
        Self {
            app,
            request_handles: Rc::new(RefCell::new(HashMap::new())),
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
}

/// A native Runner startup or lane-lifecycle failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplicatedRunnerError {
    /// The immutable Plan failed validation before any lane started.
    InvalidPlan { detail: String },
    /// One lane could not construct or start its Kernel replica.
    LaneStartup { lane: String, detail: String },
    /// A generated request Capability was not registered with the native transfer catalog.
    MissingCrossLaneRequestTransfer { capability: String },
    /// A lane stopped accepting Runner commands unexpectedly.
    LaneUnavailable { lane: String },
    /// A lane thread panicked while stopping.
    LanePanicked { lane: String },
    /// One Kernel replica did not stop cleanly.
    LaneShutdown {
        lane: String,
        outcome: ShutdownOutcome,
    },
}

impl fmt::Display for ReplicatedRunnerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPlan { detail } => {
                write!(formatter, "invalid Resolved App Plan: {detail}")
            }
            Self::LaneStartup { lane, detail } => {
                write!(
                    formatter,
                    "Execution Lane `{lane}` failed to start: {detail}"
                )
            }
            Self::MissingCrossLaneRequestTransfer { capability } => write!(
                formatter,
                "Capability `{capability}` has no registered native cross-lane request transfer"
            ),
            Self::LaneUnavailable { lane } => {
                write!(formatter, "Execution Lane `{lane}` is unavailable")
            }
            Self::LanePanicked { lane } => write!(formatter, "Execution Lane `{lane}` panicked"),
            Self::LaneShutdown { lane, outcome } => write!(
                formatter,
                "Execution Lane `{lane}` stopped with {outcome:?}"
            ),
        }
    }
}

impl std::error::Error for ReplicatedRunnerError {}

/// One native App executed as a fixed set of Plan-declared single-owner Kernel lanes.
pub struct ReplicatedNativeApp {
    plan: Arc<ResolvedAppPlan>,
    lanes: BTreeMap<ExecutionLaneId, LaneHandle>,
    diagnostics: Arc<LaneDiagnosticsState>,
    epoch: Instant,
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
        Self::start_with_transfers(plan, adapters, CrossLaneRequestCatalog::new())
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
        plan.validate()
            .map_err(|error| ReplicatedRunnerError::InvalidPlan {
                detail: error.to_string(),
            })?;
        transfers.validate_plan(&plan)?;
        let plan = Arc::new(plan);
        let adapters = Arc::new(adapters);
        let diagnostics = Arc::new(LaneDiagnosticsState::new(Arc::clone(&plan)));
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
                    run_lane(
                        thread_lane,
                        lane_plan,
                        receiver,
                        shutdown_request,
                        started,
                        lane_adapters,
                        proxy_adapter,
                        lane_diagnostics,
                        epoch,
                    );
                }) {
                Ok(thread) => thread,
                Err(error) => {
                    drop(receivers);
                    drop(routes);
                    drop(senders);
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

        for (lane, startup) in startups {
            match startup.recv() {
                Ok(Ok(())) => {}
                Ok(Err(detail)) => {
                    drop(routes);
                    drop(senders);
                    stop_lanes(lanes);
                    return Err(ReplicatedRunnerError::LaneStartup {
                        lane: lane.to_string(),
                        detail,
                    });
                }
                Err(_) => {
                    drop(routes);
                    drop(senders);
                    stop_lanes(lanes);
                    return Err(ReplicatedRunnerError::LaneUnavailable {
                        lane: lane.to_string(),
                    });
                }
            }
        }

        Ok(Self {
            plan,
            lanes,
            diagnostics,
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
        let _ = singular_binding::<C>(&self.plan, caller_instance)?;
        let consumer = self.plan.module_instance(caller_instance).ok_or_else(|| {
            RuntimeFailure::InvalidResolvedPlan {
                detail: format!("binding consumer `{caller_instance}` is absent from the Plan"),
            }
        })?;
        let lane =
            self.lanes
                .get(consumer.execution_lane())
                .ok_or_else(|| RuntimeFailure::Internal {
                    detail: format!(
                        "Execution Lane `{}` is unavailable",
                        consumer.execution_lane()
                    ),
                })?;
        let caller_instance = caller_instance.to_owned();
        let operation = operation.to_owned();
        let deadline = options
            .timeout
            .map(|timeout| self.epoch.elapsed().saturating_add(timeout));
        let (completed, completion) = oneshot::channel();
        lane.commands
            .send(Box::new(move |lane| {
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
            }))
            .await
            .map_err(|_| RuntimeFailure::Internal {
                detail: format!("Execution Lane `{}` is unavailable", lane.id),
            })?;
        completion.await.map_err(|_| RuntimeFailure::Internal {
            detail: format!("Execution Lane `{}` dropped an invocation", lane.id),
        })?
    }

    /// Stops every Kernel replica with the same bounded shutdown timeout.
    pub async fn shutdown(self, timeout: Duration) -> Result<(), ReplicatedRunnerError> {
        let mut completions = Vec::new();
        let mut threads = Vec::new();
        let mut first_error = None;
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

fn singular_binding<'a, C: RequestCapability>(
    plan: &'a ResolvedAppPlan,
    caller_instance: &str,
) -> Result<&'a CapabilityBinding, RuntimeFailure> {
    let mut bindings = plan.capability_bindings().iter().filter(|binding| {
        binding.consumer_instance() == caller_instance && binding.capability_id() == C::ID
    });
    let Some(binding) = bindings.next() else {
        return Err(RuntimeFailure::Unavailable { capability: C::ID });
    };
    let providers = 1 + bindings.count();
    if providers == 1 {
        Ok(binding)
    } else {
        Err(RuntimeFailure::AmbiguousBinding {
            capability: C::ID,
            providers,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn run_lane<F>(
    lane: ExecutionLaneId,
    plan: ResolvedAppPlan,
    mut commands: mpsc::Receiver<LaneTask>,
    mut shutdown: oneshot::Receiver<LaneShutdown>,
    started: std_mpsc::SyncSender<Result<(), String>>,
    adapters: Arc<F>,
    proxy_adapter: LaneProxyAdapter,
    diagnostics: Arc<LaneDiagnosticsState>,
    epoch: Instant,
) where
    F: Fn(&ExecutionLaneId) -> ExecutionAdapterCatalog + Send + Sync + 'static,
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
        let catalog = match adapters(&lane).with_adapter(proxy_adapter) {
            Ok(catalog) => catalog,
            Err(error) => {
                let _ = started.send(Err(error.to_string()));
                return;
            }
        };
        let driver = TokioDriver::with_epoch(epoch);
        let runtime_diagnostics = RuntimeDiagnostics::new().with_invocation_probe(Rc::new(
            LaneInvocationProbe::new(Arc::clone(&diagnostics), lane.clone()),
        ));
        let app = match lenso_kernel::Kernel::start_with_diagnostics(
            plan,
            driver,
            catalog,
            runtime_diagnostics,
        )
        .await
        {
            Ok(app) => app,
            Err(error) => {
                let _ = started.send(Err(format!("{error:?}")));
                return;
            }
        };
        let lane_runtime = LaneRuntime::new(app.clone());
        let _ = started.send(Ok(()));
        diagnostics.publish_lane(&lane, &app, cpu_started.elapsed());
        let mut sample_interval = tokio::time::interval(Duration::from_millis(10));

        loop {
            tokio::select! {
                biased;
                shutdown = &mut shutdown => {
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
                command = commands.recv() => if let Some(task) = command {
                    task(lane_runtime.clone());
                } else {
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
