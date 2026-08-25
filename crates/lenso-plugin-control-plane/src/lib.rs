//! Dynamic Plugin authority, content Store, resolution, and Generation supervision.

mod canonical;
mod controller;
mod durable;
mod durable_supervisor;
mod kernel_runtime;
mod model;
mod multi_execution;
mod replicated_runtime;
mod resolver;
mod store;
mod supervisor;

pub use canonical::{CanonicalDocument, ControlPlaneError, sha256_digest, strict_json};
pub use controller::{GenerationController, GenerationControllerClient, GenerationControllerEvent};
pub use durable::{
    ActivationDirection, ControlHealth, ControlLifecycle, ControlStateStore, DurableControlState,
    FileControlStateStore, GenerationControlRecord, MemoryControlStateStore, RetirementReason,
};
pub use durable_supervisor::{
    DurableGenerationLease, DurableGenerationRoute, DurableGenerationSupervisor,
    DurableTransitionOutcome, GenerationFailureOutcome, GenerationMaintenanceOutcome,
};
pub use kernel_runtime::{CatalogFactory, KernelGenerationHandle, KernelGenerationRuntime};
pub use lenso_plugin_bundle::{
    ArtifactDeclaration, ArtifactKind, BindingTemplate, CapabilityDeclaration,
    CapabilityRequirement, DataContribution, ImplementationVariant, ModuleContribution,
    PermissionRequest, PluginFeature, PluginManifest, ProductMetadataDeclaration,
    RequirementCardinality, StateDeclaration, SupportChannel, TrustLevel,
};
pub use model::*;
pub use multi_execution::MultiExecutionCatalogFactory;
pub use replicated_runtime::{
    ReplicatedCatalogFactory, ReplicatedGenerationHandle, ReplicatedGenerationRuntime,
};
pub use resolver::{ResolutionInput, ResolvedGeneration, resolve_generation};
pub use store::{AdmissionPolicy, AdmissionReceipt, AdmittedArtifact, PluginBundle, PluginStore};
pub use supervisor::{
    GenerationLease, GenerationRuntime, GenerationStatus, GenerationSupervisor, TransitionOutcome,
};
