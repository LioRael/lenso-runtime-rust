//! Framework-level lifecycle for one durable Lenso Host.
//!
//! Products retain authority over App resolution, Plugin selection, and recovery
//! inputs. This module owns the common Controller task, fenced routes, and exact
//! suspend or retirement handshake.

use std::{collections::BTreeMap, time::Duration};

pub use lenso_plugin_control_plane::{
    AppGenerationTransitionSpec, CanonicalDocument, ControlPlaneError, ControlStateStore,
    DurableControlState, DurableGenerationRoute, DurableTransitionOutcome,
    GenerationControllerClient, GenerationControllerEvent, GenerationRuntime, ResolvedGeneration,
    StateCompatibilityReceipt,
};
use lenso_plugin_control_plane::{DurableGenerationSupervisor, GenerationController};

pub use lenso_plugin_control_plane::{
    CatalogFactory, FileControlStateStore, KernelGenerationRuntime, MemoryControlStateStore,
    MultiExecutionCatalogFactory,
};

const DEFAULT_MAINTENANCE_INTERVAL: Duration = Duration::from_millis(100);

pub mod control;

/// Configures one framework Host without assuming a product, Profile, or surface.
#[derive(Debug)]
pub struct HostBuilder<R, S> {
    app_id: String,
    runtime: R,
    store: S,
    maintenance_interval: Duration,
}

impl<R, S> HostBuilder<R, S>
where
    R: GenerationRuntime + 'static,
    S: ControlStateStore + 'static,
    R::Route: 'static,
{
    /// Creates a Builder from the product's exact App identity, runtime, and durable Store.
    pub fn new(app_id: impl Into<String>, runtime: R, store: S) -> Self {
        Self {
            app_id: app_id.into(),
            runtime,
            store,
            maintenance_interval: DEFAULT_MAINTENANCE_INTERVAL,
        }
    }

    /// Sets how frequently terminal failures, drains, and rollback windows are maintained.
    #[must_use]
    pub fn maintenance_interval(mut self, interval: Duration) -> Self {
        self.maintenance_interval = interval;
        self
    }

    /// Builds a fresh Host. Live durable state must use [`Self::recover`] or
    /// [`Self::replace_suspended`].
    pub fn build(self) -> Result<Host<R::Route>, ControlPlaneError> {
        let Self {
            app_id,
            runtime,
            store,
            maintenance_interval,
        } = self;
        let supervisor = DurableGenerationSupervisor::open(app_id, runtime, store)?;
        Self::start(supervisor, maintenance_interval)
    }

    /// Restages the exact live Generations supplied by product-owned recovery authority.
    pub async fn recover(
        self,
        generations: &BTreeMap<String, ResolvedGeneration>,
        now_unix_nanos: u128,
    ) -> Result<Host<R::Route>, ControlPlaneError> {
        let Self {
            app_id,
            runtime,
            store,
            maintenance_interval,
        } = self;
        let supervisor = DurableGenerationSupervisor::recover(
            app_id,
            runtime,
            store,
            generations,
            now_unix_nanos,
        )
        .await?;
        Self::start(supervisor, maintenance_interval)
    }

    /// Starts a replacement Host after the previous incarnation proved clean suspension.
    pub fn replace_suspended(self) -> Result<Host<R::Route>, ControlPlaneError> {
        let Self {
            app_id,
            runtime,
            store,
            maintenance_interval,
        } = self;
        let supervisor =
            DurableGenerationSupervisor::replace_suspended_host(app_id, runtime, store)?;
        Self::start(supervisor, maintenance_interval)
    }

    fn start(
        supervisor: DurableGenerationSupervisor<R, S>,
        maintenance_interval: Duration,
    ) -> Result<Host<R::Route>, ControlPlaneError> {
        let (controller, client) = GenerationController::new(supervisor, maintenance_interval)?;
        let task = tokio::task::spawn_local(controller.run());
        Ok(Host {
            client,
            task: Some(task),
        })
    }
}

/// A running Controller and its cloneable, fenced Host interface.
///
/// The caller must finish with [`Self::drain_and_suspend`] (or [`Self::suspend`]
/// when already quiescent) for process replacement or
/// [`Self::shutdown`] for terminal retirement.
#[derive(Debug)]
#[must_use = "a running Host must be suspended or shut down"]
pub struct Host<T: Clone + std::fmt::Debug> {
    client: GenerationControllerClient<T>,
    task: Option<tokio::task::JoinHandle<Result<DurableControlState, ControlPlaneError>>>,
}

impl<T> Host<T>
where
    T: Clone + std::fmt::Debug + 'static,
{
    /// Returns a cloneable Controller interface for product-owned reconciliation loops.
    pub fn controller(&self) -> GenerationControllerClient<T> {
        self.client.clone()
    }

    /// Subscribes to lifecycle events without granting lifecycle authority.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<GenerationControllerEvent> {
        self.client.subscribe()
    }

    /// Pins the current route and fencing epochs for one complete product operation.
    pub async fn route(&self) -> Result<DurableGenerationRoute<T>, ControlPlaneError> {
        self.client.route().await
    }

    /// Returns the durable state observed by the single Controller authority.
    pub async fn inspect(&self) -> Result<DurableControlState, ControlPlaneError> {
        self.client.inspect().await
    }

    /// Stages and atomically activates one exact resolved Generation.
    pub async fn transition(
        &self,
        transition: CanonicalDocument<AppGenerationTransitionSpec>,
        candidate: ResolvedGeneration,
        receipts: BTreeMap<String, CanonicalDocument<StateCompatibilityReceipt>>,
    ) -> Result<DurableTransitionOutcome, ControlPlaneError> {
        self.client
            .transition(transition, candidate, receipts)
            .await
    }

    /// Suspends this process incarnation while retaining durable Generation authority.
    pub async fn suspend(&mut self) -> Result<DurableControlState, ControlPlaneError> {
        let expected = self.client.suspend().await?;
        self.join_consistent(expected).await
    }

    /// Fences new work and drains before a restartable suspension. The first
    /// caller's budget is shared across lease drain and Generation cleanup.
    /// This is not OS process-termination proof; an error requires outer ownership
    /// handling before another Host can recover the durable state.
    pub async fn drain_and_suspend(
        &mut self,
        timeout: Duration,
    ) -> Result<DurableControlState, ControlPlaneError> {
        let result = self.client.drain_and_suspend(timeout).await;
        match result {
            Ok(expected) if self.task.is_some() => self.join_consistent(expected).await,
            outcome => outcome,
        }
    }

    /// Retires all Generations and stops this Host permanently.
    pub async fn shutdown(&mut self) -> Result<DurableControlState, ControlPlaneError> {
        let expected = self.client.shutdown().await?;
        self.join_consistent(expected).await
    }

    async fn join_consistent(
        &mut self,
        expected: DurableControlState,
    ) -> Result<DurableControlState, ControlPlaneError> {
        let task = self
            .task
            .take()
            .ok_or_else(|| ControlPlaneError::HostFailure {
                detail: "Host Controller is already stopped".to_owned(),
            })?;
        let actual = task
            .await
            .map_err(|error| ControlPlaneError::HostFailure {
                detail: format!("Host Controller task failed: {error}"),
            })??;
        if actual != expected {
            return Err(ControlPlaneError::HostFailure {
                detail: "Host Controller returned inconsistent durable state".to_owned(),
            });
        }
        Ok(actual)
    }
}

#[cfg(test)]
mod tests {
    use futures::FutureExt;
    use lenso_plugin_control_plane::{GenerationRuntime, MemoryControlStateStore};

    use super::*;

    #[derive(Debug)]
    struct EmptyRuntime;

    impl GenerationRuntime for EmptyRuntime {
        type Handle = ();
        type Route = ();

        fn stage<'a>(
            &'a mut self,
            _generation: &'a ResolvedGeneration,
            _ready_timeout_nanos: u64,
        ) -> futures::future::LocalBoxFuture<'a, Result<Self::Handle, ControlPlaneError>> {
            async { Ok(()) }.boxed_local()
        }

        fn shutdown(
            &mut self,
            _handle: Self::Handle,
            _drain_timeout_nanos: u64,
        ) -> futures::future::LocalBoxFuture<'_, Result<(), ControlPlaneError>> {
            async { Ok(()) }.boxed_local()
        }

        fn terminal_failure(&self, _handle: &Self::Handle) -> Option<ControlPlaneError> {
            None
        }

        fn route(&self, _handle: &Self::Handle) -> Self::Route {}
    }

    #[tokio::test(flavor = "current_thread")]
    async fn builder_owns_controller_and_exact_suspend_handshake() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut host = HostBuilder::new(
                    "example.app",
                    EmptyRuntime,
                    MemoryControlStateStore::default(),
                )
                .build()
                .expect("fresh Host should start");

                let running = host.inspect().await.expect("Host should inspect");
                assert!(!running.host_suspended);

                let suspended = host.suspend().await.expect("Host should suspend");
                assert!(suspended.host_suspended);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn drain_suspend_rejects_zero_budget_and_replays_completed_handshake() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut host = HostBuilder::new(
                    "example.app",
                    EmptyRuntime,
                    MemoryControlStateStore::default(),
                )
                .build()
                .unwrap();
                assert!(host.drain_and_suspend(Duration::ZERO).await.is_err());
                assert!(!host.inspect().await.unwrap().host_suspended);
                let state = host
                    .drain_and_suspend(Duration::from_secs(1))
                    .await
                    .unwrap();
                assert!(state.host_suspended);
                assert_eq!(
                    host.drain_and_suspend(Duration::from_secs(2))
                        .await
                        .unwrap(),
                    state
                );
            })
            .await;
    }
}
