//! The stable Rust authoring interface for Lenso Plugins and built-in Modules.
//!
//! Harness feature authors use the Plugin-named facade. Built-in App authors
//! may retain the Module-named compatibility facade. Adapter and Kernel
//! implementation details remain behind generated glue.

mod provider_stream;
mod typed_extension;

pub use lenso_module_authoring::{
    BoundCapabilityClient, CapabilityClient, CapabilityClientMany, ManyPort, ModuleError, Port,
};
pub use lenso_native_adapter::{
    Lifecycle, ManagedTasks, ManagedTasksError, ModuleConfig, PluginConfig, module, plugin,
    provides,
};

pub use provider_stream::{ProviderStream, ProviderStreamChannel, StreamInput};
pub use typed_extension::{CtxExt, TypedExtension, TypedExtensionError};

pub use lenso_kernel::{
    ActivateContext, CancellationToken, DeactivateContext, InvocationContext as Ctx,
    ManagedTaskScope, ModuleFuture, PrepareContext, RuntimeFailure,
};

/// A Module operation result that can explicitly preserve Domain and Runtime failures.
pub type ModuleResult<T, DomainError> = Result<T, ModuleError<DomainError, RuntimeFailure>>;

/// Plugin-named operation failure preserving Domain and Runtime failures.
pub type PluginError<DomainError> = ModuleError<DomainError, RuntimeFailure>;

/// A Plugin operation result preserving Domain and Runtime failures.
pub type PluginResult<T, DomainError> = Result<T, PluginError<DomainError>>;

/// An asynchronous Event handler outcome observed by the Adapter after admission.
///
/// Event handlers have no publisher-visible Domain result. Returning a Runtime Failure lets the
/// Adapter report diagnostics and apply Module supervision after the Event was admitted.
pub type ModuleEventResult = Result<(), RuntimeFailure>;

/// A Plugin Event handler outcome observed by the Adapter after admission.
pub type PluginEventResult = ModuleEventResult;

/// Common imports for native Rust Plugin and built-in Module authors.
pub mod prelude {
    pub use crate::{
        ActivateContext, BoundCapabilityClient, CancellationToken, CapabilityClient,
        CapabilityClientMany, Ctx, CtxExt, DeactivateContext, Lifecycle, ManagedTaskScope,
        ManagedTasks, ManagedTasksError, ManyPort, ModuleConfig, ModuleError, ModuleEventResult,
        ModuleFuture, ModuleResult, PluginConfig, PluginError, PluginEventResult, PluginResult,
        Port, PrepareContext, ProviderStream, ProviderStreamChannel, RuntimeFailure, StreamInput,
        TypedExtension, TypedExtensionError, module, plugin, provides,
    };
}

/// Implementation details used only by generated Module glue.
#[doc(hidden)]
pub mod __private {
    pub use lenso_native_adapter::__private::*;
}
