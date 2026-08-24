//! Dynamic Plugin authority, content Store, resolution, and Generation supervision.

mod canonical;
mod durable;
mod durable_supervisor;
mod kernel_runtime;
mod model;
mod multi_execution;
mod resolver;
mod store;
mod supervisor;

pub use canonical::{CanonicalDocument, ControlPlaneError, sha256_digest, strict_json};
pub use durable::{
    ActivationDirection, ControlHealth, ControlLifecycle, ControlStateStore, DurableControlState,
    FileControlStateStore, GenerationControlRecord, MemoryControlStateStore,
};
pub use durable_supervisor::{
    DurableGenerationLease, DurableGenerationSupervisor, DurableTransitionOutcome,
};
pub use kernel_runtime::{CatalogFactory, KernelGenerationHandle, KernelGenerationRuntime};
pub use model::*;
pub use multi_execution::MultiExecutionCatalogFactory;
pub use resolver::{ResolutionInput, ResolvedGeneration, resolve_generation};
pub use store::{AdmissionPolicy, AdmissionReceipt, AdmittedArtifact, PluginBundle, PluginStore};
pub use supervisor::{
    GenerationLease, GenerationRuntime, GenerationStatus, GenerationSupervisor, TransitionOutcome,
};
