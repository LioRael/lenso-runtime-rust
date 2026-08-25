//! The stable Rust authoring Interface for Lenso vNext Modules.
//!
//! Module authors depend on this crate and their generated Capability crates;
//! Adapter and Kernel implementation details remain behind generated glue.

pub use lenso_module_authoring::{CapabilityClient, Port};
pub use lenso_native_adapter::{ModuleConfig, module, provides};

pub use lenso_kernel::{
    ActivateContext, DeactivateContext, InvocationContext as Ctx, ManagedTaskScope, ModuleFuture,
    PrepareContext, RuntimeFailure,
};

/// Common imports for native Rust Module authors.
pub mod prelude {
    pub use crate::{
        ActivateContext, CapabilityClient, Ctx, DeactivateContext, ManagedTaskScope, ModuleConfig,
        ModuleFuture, Port, PrepareContext, RuntimeFailure, module, provides,
    };
}

/// Implementation details used only by generated Module glue.
#[doc(hidden)]
pub mod __private {
    pub use lenso_native_adapter::__private::*;
}
