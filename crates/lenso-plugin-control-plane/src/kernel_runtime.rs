use std::{rc::Rc, time::Duration};

use lenso_kernel::{ExecutionAdapterCatalog, NativeApp, ShutdownOutcome};
use lenso_runner::TokioDriver;

use crate::{ControlPlaneError, GenerationRuntime, ResolvedGeneration};

/// Exact catalog assembly callback owned by the product Host Build.
pub trait CatalogFactory: 'static {
    /// Assembles exactly one installed Adapter per selected execution class.
    fn catalog(
        &self,
        generation: &ResolvedGeneration,
    ) -> Result<ExecutionAdapterCatalog, ControlPlaneError>;
}

/// Actual Kernel generation retained by the Supervisor until drain completes.
#[derive(Debug)]
pub struct KernelGenerationHandle {
    app: NativeApp,
    driver: TokioDriver,
}

/// Production lane-local Generation runtime backed by Kernel and `TokioDriver`.
pub struct KernelGenerationRuntime<F: CatalogFactory> {
    factory: Rc<F>,
}

impl<F: CatalogFactory> KernelGenerationRuntime<F> {
    /// Creates a runtime from the product's exact Host Build catalog assembly.
    pub fn new(factory: F) -> Self {
        Self {
            factory: Rc::new(factory),
        }
    }
}

impl<F: CatalogFactory> std::fmt::Debug for KernelGenerationRuntime<F> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KernelGenerationRuntime")
            .finish_non_exhaustive()
    }
}

impl<F: CatalogFactory> GenerationRuntime for KernelGenerationRuntime<F> {
    type Handle = KernelGenerationHandle;
    type Route = NativeApp;

    fn stage<'a>(
        &'a mut self,
        generation: &'a ResolvedGeneration,
        ready_timeout_nanos: u64,
    ) -> futures::future::LocalBoxFuture<'a, Result<Self::Handle, ControlPlaneError>> {
        Box::pin(async move {
            let catalog = self.factory.catalog(generation)?;
            let driver = TokioDriver::new();
            let app = tokio::time::timeout(
                Duration::from_nanos(ready_timeout_nanos),
                lenso_kernel::Kernel::start(generation.plan.clone(), driver.clone(), catalog),
            )
            .await
            .map_err(|_| ControlPlaneError::HostFailure {
                detail: "Kernel Generation Ready Gate timed out".to_owned(),
            })?
            .map_err(|error| ControlPlaneError::HostFailure {
                detail: format!("Kernel Generation failed before Ready: {error:?}"),
            })?;
            Ok(KernelGenerationHandle { app, driver })
        })
    }

    fn shutdown(
        &mut self,
        handle: Self::Handle,
        drain_timeout_nanos: u64,
    ) -> futures::future::LocalBoxFuture<'_, Result<(), ControlPlaneError>> {
        Box::pin(async move {
            handle.driver.request_shutdown();
            match handle
                .app
                .shutdown(Duration::from_nanos(drain_timeout_nanos))
                .await
            {
                ShutdownOutcome::Clean => Ok(()),
                ShutdownOutcome::RuntimeFailure { error } => Err(ControlPlaneError::HostFailure {
                    detail: format!("Generation cleanup failed: {error:?}"),
                }),
                ShutdownOutcome::Timeout => Err(ControlPlaneError::HostFailure {
                    detail: "Generation drain timed out".to_owned(),
                }),
            }
        })
    }

    fn terminal_failure(&self, handle: &Self::Handle) -> Option<ControlPlaneError> {
        handle
            .app
            .terminal_failure()
            .map(|error| ControlPlaneError::HostFailure {
                detail: format!("Kernel Generation failed after Ready: {error:?}"),
            })
    }

    fn route(&self, handle: &Self::Handle) -> Self::Route {
        handle.app.clone()
    }
}
