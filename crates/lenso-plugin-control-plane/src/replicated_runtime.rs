use std::{sync::Arc, time::Duration};

use lenso_app_plan::ExecutionLaneId;
use lenso_kernel::ExecutionAdapterCatalog;
use lenso_runner::{CrossLaneTransferCatalog, ReplicatedAppRoute, ReplicatedNativeApp};

use crate::{ControlPlaneError, GenerationRuntime, ResolvedGeneration};

/// Thread-safe Host Build factory for one lane-local Adapter catalog.
pub trait ReplicatedCatalogFactory: Send + Sync + 'static {
    fn catalog(
        &self,
        generation: &ResolvedGeneration,
        lane: &ExecutionLaneId,
    ) -> Result<ExecutionAdapterCatalog, ControlPlaneError>;
}

/// Complete Plan Lane Set retained until Generation drain finishes.
#[derive(Debug)]
pub struct ReplicatedGenerationHandle {
    app: ReplicatedNativeApp,
}

/// Production Generation runtime which makes all declared lanes one Ready/failure unit.
pub struct ReplicatedGenerationRuntime<F: ReplicatedCatalogFactory> {
    factory: Arc<F>,
    transfers: CrossLaneTransferCatalog,
}

impl<F: ReplicatedCatalogFactory> ReplicatedGenerationRuntime<F> {
    pub fn new(factory: F) -> Self {
        Self {
            factory: Arc::new(factory),
            transfers: CrossLaneTransferCatalog::new(),
        }
    }

    #[must_use]
    pub fn with_transfers(mut self, transfers: CrossLaneTransferCatalog) -> Self {
        self.transfers = transfers;
        self
    }
}

impl<F: ReplicatedCatalogFactory> std::fmt::Debug for ReplicatedGenerationRuntime<F> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReplicatedGenerationRuntime")
            .finish_non_exhaustive()
    }
}

impl<F: ReplicatedCatalogFactory> GenerationRuntime for ReplicatedGenerationRuntime<F> {
    type Handle = ReplicatedGenerationHandle;
    type Route = ReplicatedAppRoute;

    fn stage<'a>(
        &'a mut self,
        generation: &'a ResolvedGeneration,
        ready_timeout_nanos: u64,
    ) -> futures::future::LocalBoxFuture<'a, Result<Self::Handle, ControlPlaneError>> {
        Box::pin(async move {
            let factory = Arc::clone(&self.factory);
            let authority = Arc::new(generation.clone());
            let lane_authority = Arc::clone(&authority);
            let app = ReplicatedNativeApp::start_with_fallible_transfer_catalog(
                generation.plan.clone(),
                move |lane| {
                    factory
                        .catalog(&lane_authority, lane)
                        .map_err(|error| error.to_string())
                },
                self.transfers.clone(),
                Some(Duration::from_nanos(ready_timeout_nanos)),
            )
            .map_err(|error| ControlPlaneError::HostFailure {
                detail: format!("replicated Kernel Generation failed before Ready: {error}"),
            })?;
            Ok(ReplicatedGenerationHandle { app })
        })
    }

    fn shutdown(
        &mut self,
        handle: Self::Handle,
        drain_timeout_nanos: u64,
    ) -> futures::future::LocalBoxFuture<'_, Result<(), ControlPlaneError>> {
        Box::pin(async move {
            let terminal = handle.app.terminal_failure();
            match handle
                .app
                .shutdown(Duration::from_nanos(drain_timeout_nanos))
                .await
            {
                Ok(()) => Ok(()),
                Err(error) if terminal.as_ref() == Some(&error) => Ok(()),
                Err(error) => Err(ControlPlaneError::HostFailure {
                    detail: format!("replicated Generation cleanup failed: {error}"),
                }),
            }
        })
    }

    fn terminal_failure(&self, handle: &Self::Handle) -> Option<ControlPlaneError> {
        handle
            .app
            .terminal_failure()
            .map(|error| ControlPlaneError::HostFailure {
                detail: format!("replicated Kernel Generation failed after Ready: {error}"),
            })
    }

    fn route(&self, handle: &Self::Handle) -> Self::Route {
        handle.app.route()
    }
}
