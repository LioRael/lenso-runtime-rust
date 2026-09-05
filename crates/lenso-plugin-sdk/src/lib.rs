//! Execution-target lowering for portable Rust Plugins.
//!
//! Product SDKs own Capability semantics and typed authoring. This crate owns
//! only the final JSON request seam and lowers it to Wasm Component or Process
//! transport glue. Plugin authors normally use a product SDK instead of this
//! interface directly.

use serde_json::Value;

pub use lenso_plugin_sdk_macros::plugin;

#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub mod __wasm {
    wit_bindgen::generate!({
        inline: r#"
            package lenso:runtime@1.0.0;

            world plugin {
              export describe: func() -> string;
              export invoke: func(capability: string, operation: string, request-json: string) -> result<string, string>;
            }
        "#,
        world: "plugin",
        export_macro_name: "export_lenso_plugin",
        pub_export_macro: true,
        default_bindings_module: "::lenso::__wasm",
    });
}

/// Transport-neutral result produced by a product SDK's generated dispatcher.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub enum InvocationOutcome {
    /// A successful Capability response.
    Success(Value),
    /// A domain error defined by the Capability contract.
    DomainError(Value),
    /// An adapter-safe implementation failure.
    Failure(String),
}

/// JSON request seam implemented by generated product SDK glue.
///
/// This interface deliberately has no knowledge of Agent Tools, Ingress,
/// authentication, or any other product Capability.
#[doc(hidden)]
pub trait JsonRequestHandler: Default {
    /// Handles one validated Capability request.
    fn invoke(&self, capability: &str, operation: &str, request: Value) -> InvocationOutcome;
}

/// Lowers a generated JSON request handler to Wasm Component or Process.
///
/// Product SDK macros call this macro; Plugin business code should not need to.
#[doc(hidden)]
#[macro_export]
macro_rules! __export_json_request_handler {
    (
        $plugin:ty {
            capability_id: $capability_id:literal,
            descriptor_version: $descriptor_version:literal,
            descriptor_digest: $descriptor_digest:literal,
            requests: [$first_request:literal $(, $request:literal)* $(,)?] $(,)?
        }
    ) => {
        type __LensoExportedPlugin = $plugin;

        #[cfg(target_arch = "wasm32")]
        mod __lenso_wasm_export {
            struct Component;

            const DESCRIPTOR: &str = $crate::__private::lenso_guest_sdk::__request_plugin_descriptor!(
                $capability_id,
                $descriptor_version,
                $first_request $(, $request)*
            );

            #[used]
            #[unsafe(link_section = "lenso.plugin-descriptor.v1")]
            static DESCRIPTOR_SECTION: [u8; DESCRIPTOR.len()] =
                $crate::__private::lenso_guest_sdk::__descriptor_bytes(DESCRIPTOR);

            impl $crate::__private::wasm::Guest for Component {
                fn describe() -> ::std::string::String {
                    DESCRIPTOR.to_owned()
                }

                fn invoke(
                    capability: ::std::string::String,
                    operation: ::std::string::String,
                    request_json: ::std::string::String,
                ) -> ::std::result::Result<::std::string::String, ::std::string::String> {
                    let request = $crate::__private::serde_json::from_str(&request_json)
                        .map_err(|_| "\"invalid_arguments\"".to_owned())?;
                    match <super::__LensoExportedPlugin as $crate::JsonRequestHandler>::invoke(
                        &<super::__LensoExportedPlugin as ::std::default::Default>::default(),
                        &capability,
                        &operation,
                        request,
                    ) {
                        $crate::InvocationOutcome::Success(value) =>
                            $crate::__private::serde_json::to_string(&value)
                                .map_err(|error| error.to_string()),
                        $crate::InvocationOutcome::DomainError(value) =>
                            ::std::result::Result::Err(
                                $crate::__private::serde_json::to_string(&value)
                                    .unwrap_or_else(|_| "\"execution_failed\"".to_owned()),
                            ),
                        $crate::InvocationOutcome::Failure(detail) =>
                            ::std::result::Result::Err(
                                $crate::__private::serde_json::to_string(&detail)
                                    .unwrap_or_else(|_| "\"execution_failed\"".to_owned()),
                            ),
                    }
                }
            }

            $crate::__private::wasm::export_lenso_plugin!(Component);
        }

        #[cfg(not(target_arch = "wasm32"))]
        mod __lenso_process_export {
            struct Component;

            const DESCRIPTOR: &str =
                $crate::__private::lenso_guest_sdk::__request_plugin_descriptor!(
                    $capability_id,
                    $descriptor_version,
                    digest: $descriptor_digest,
                    $first_request $(, $request)*
                );

            impl $crate::__private::lenso_process_sdk::ProcessPluginV2 for Component {
                type Instance = super::__LensoExportedPlugin;

                fn construct(
                    &self,
                    _params: &$crate::__private::lenso_process_sdk::authoring::ConstructParams,
                    _context: $crate::__private::lenso_process_sdk::ProcessLifecycleContext,
                ) -> ::std::result::Result<Self::Instance, ::std::string::String> {
                    Ok(<super::__LensoExportedPlugin as ::std::default::Default>::default())
                }

                fn invoke(
                    &self,
                    instance: &Self::Instance,
                    params: $crate::__private::lenso_process_sdk::authoring::InvokeParams,
                    _context: $crate::__private::lenso_process_sdk::ProcessInvocationContext,
                ) -> $crate::__private::lenso_process_sdk::authoring::InvocationOutcome {
                    match <super::__LensoExportedPlugin as $crate::JsonRequestHandler>::invoke(
                        instance,
                        &params.capability_id,
                        &params.operation,
                        params.payload,
                    ) {
                        $crate::InvocationOutcome::Success(value) =>
                            $crate::__private::lenso_process_sdk::authoring::InvocationOutcome::Success {
                                value,
                            },
                        $crate::InvocationOutcome::DomainError(value) =>
                            $crate::__private::lenso_process_sdk::authoring::InvocationOutcome::Domain {
                                error: value,
                            },
                        $crate::InvocationOutcome::Failure(detail) =>
                            $crate::__private::lenso_process_sdk::authoring::InvocationOutcome::Runtime {
                                failure: $crate::__private::lenso_process_sdk::authoring::RuntimeFailure::PluginFailure {
                                    detail,
                                },
                            },
                    }
                }
            }

            pub fn serve() {
                if ::std::env::args_os().nth(1).as_deref()
                    == Some(::std::ffi::OsStr::new("--lenso-describe"))
                {
                    println!("{DESCRIPTOR}");
                    return;
                }
                $crate::__private::lenso_process_sdk::serve_v2(Component)
                .expect("serve Lenso Process V2 Plugin");
            }
        }

        #[cfg(not(target_arch = "wasm32"))]
        fn main() {
            __lenso_process_export::serve();
        }
    };
}

#[doc(hidden)]
pub mod __private {
    pub use serde_json;

    #[cfg(target_arch = "wasm32")]
    pub use crate::__wasm as wasm;
    pub use lenso_guest_sdk;
    #[cfg(not(target_arch = "wasm32"))]
    pub use lenso_process_sdk;
}
