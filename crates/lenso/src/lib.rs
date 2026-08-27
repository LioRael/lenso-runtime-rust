//! The stable Rust authoring interface for Lenso Plugins.
//!
//! Adapter and Kernel implementation details remain behind generated glue.

mod provider_stream;
mod typed_extension;

pub use lenso_native_adapter::{
    Lifecycle, ManagedTasks, ManagedTasksError, PluginConfig, plugin, provides,
};
use lenso_plugin_authoring::PluginError as AuthoringPluginError;
pub use lenso_plugin_authoring::{
    BoundCapabilityClient, CapabilityClient, CapabilityClientMany, ManyPort, Port,
};

pub use provider_stream::{ProviderStream, ProviderStreamChannel, StreamInput};
pub use typed_extension::{CtxExt, TypedExtension, TypedExtensionError};

pub use lenso_kernel::{
    ActivateContext, CancellationToken, DeactivateContext, InvocationContext as Ctx,
    ManagedTaskScope, PluginFuture, PrepareContext, RuntimeFailure,
};

/// An operation failure preserving Domain and Runtime failures.
pub type PluginError<DomainError> = AuthoringPluginError<DomainError, RuntimeFailure>;

/// A Plugin operation result preserving Domain and Runtime failures.
pub type PluginResult<T, DomainError> = Result<T, PluginError<DomainError>>;

/// An asynchronous Event handler outcome observed by the Adapter after admission.
///
/// Event handlers have no publisher-visible Domain result. Returning a Runtime Failure lets the
/// Adapter report diagnostics and apply Plugin supervision after the Event was admitted.
pub type PluginEventResult = Result<(), RuntimeFailure>;

/// Common imports for native Rust Plugin authors.
pub mod prelude {
    pub use crate::{
        ActivateContext, BoundCapabilityClient, CancellationToken, CapabilityClient,
        CapabilityClientMany, Ctx, CtxExt, DeactivateContext, Lifecycle, ManagedTaskScope,
        ManagedTasks, ManagedTasksError, ManyPort, PluginConfig, PluginError, PluginEventResult,
        PluginFuture, PluginResult, Port, PrepareContext, ProviderStream, ProviderStreamChannel,
        RuntimeFailure, StreamInput, TypedExtension, TypedExtensionError, plugin, provides,
    };
}

/// Implementation details used only by generated Plugin glue.
#[doc(hidden)]
pub mod __private {
    pub use lenso_native_adapter::__private::*;
}
