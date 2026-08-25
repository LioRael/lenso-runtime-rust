//! The stable Rust authoring Interface for Lenso vNext Modules.
//!
//! Module authors depend on this crate and their generated Capability crates;
//! Adapter and Kernel implementation details remain behind generated glue.

mod provider_stream;

pub use lenso_module_authoring::{
    BoundCapabilityClient, CapabilityClient, CapabilityClientMany, ManyPort, ModuleError, Port,
};
pub use lenso_native_adapter::{Lifecycle, ModuleConfig, module, provides};

pub use provider_stream::{ProviderStream, ProviderStreamChannel, StreamInput};

pub use lenso_kernel::{
    ActivateContext, DeactivateContext, InvocationContext as Ctx, ManagedTaskScope, ModuleFuture,
    PrepareContext, RuntimeFailure,
};

/// A Module operation result that can explicitly preserve Domain and Runtime failures.
pub type ModuleResult<T, DomainError> = Result<T, ModuleError<DomainError, RuntimeFailure>>;

/// An asynchronous Event handler outcome observed by the Adapter after admission.
///
/// Event handlers have no publisher-visible Domain result. Returning a Runtime Failure lets the
/// Adapter report diagnostics and apply Module supervision after the Event was admitted.
pub type ModuleEventResult = Result<(), RuntimeFailure>;

/// Common imports for native Rust Module authors.
pub mod prelude {
    pub use crate::{
        ActivateContext, BoundCapabilityClient, CapabilityClient, CapabilityClientMany, Ctx,
        DeactivateContext, Lifecycle, ManagedTaskScope, ManyPort, ModuleConfig, ModuleError,
        ModuleEventResult, ModuleFuture, ModuleResult, Port, PrepareContext, ProviderStream,
        ProviderStreamChannel, RuntimeFailure, StreamInput, module, provides,
    };
}

/// Implementation details used only by generated Module glue.
#[doc(hidden)]
pub mod __private {
    pub use lenso_native_adapter::__private::*;
}
