//! Target-independent Rust authoring for portable Lenso Plugins.
//!
//! Plugin business code implements a product-facing trait once. The export
//! macro lowers that implementation to the selected execution target at
//! compile time, keeping WIT and process framing out of the Plugin project.

use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

#[cfg(target_arch = "wasm32")]
extern crate self as lenso_plugin_sdk;

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
        default_bindings_module: "::lenso_plugin_sdk::__wasm",
    });
}

/// One target-independent request result.
#[derive(Clone, Debug)]
pub enum RequestOutcome {
    /// A successful capability response.
    Success(Value),
    /// A domain error defined by the provided capability.
    DomainError(Value),
    /// An adapter-safe implementation failure.
    Failure(String),
}

/// Business implementation for one request-based Lenso Plugin.
pub trait RequestPlugin: Default {
    /// Handles one validated capability request.
    fn invoke(&self, capability: &str, operation: &str, request: Value) -> RequestOutcome;
}

/// A typed Agent Tool exposed through `lenso.agent.tool-provider@2`.
pub trait AgentTool: Default {
    /// JSON arguments accepted by the Tool.
    type Arguments: DeserializeOwned + JsonSchema;
    /// Domain error returned by the Tool.
    type Error: Serialize;

    /// Stable Tool name presented to the model.
    const NAME: &'static str;
    /// Human-readable Tool description presented to the model.
    const DESCRIPTION: &'static str;

    /// Executes the Tool business operation.
    fn execute(&self, arguments: Self::Arguments) -> Result<String, Self::Error>;
}

#[doc(hidden)]
pub struct AgentToolAdapter<T>(T);

impl<T> std::fmt::Debug for AgentToolAdapter<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AgentToolAdapter")
    }
}

impl<T: AgentTool> Default for AgentToolAdapter<T> {
    fn default() -> Self {
        Self(T::default())
    }
}

impl<T: AgentTool> RequestPlugin for AgentToolAdapter<T> {
    fn invoke(&self, capability: &str, operation: &str, request: Value) -> RequestOutcome {
        if capability != "lenso.agent.tool-provider@2" {
            return RequestOutcome::DomainError(json!("not_found"));
        }
        match operation {
            "catalog" => catalog::<T>(),
            "execute" => execute(&self.0, &request),
            _ => RequestOutcome::DomainError(json!("not_found")),
        }
    }
}

fn catalog<T: AgentTool>() -> RequestOutcome {
    let input_schema_json = match serde_json::to_string(&schemars::schema_for!(T::Arguments)) {
        Ok(schema) => schema,
        Err(error) => return RequestOutcome::Failure(error.to_string()),
    };
    RequestOutcome::Success(json!({
        "tools": [{
            "name": T::NAME,
            "description": T::DESCRIPTION,
            "input_schema_json": input_schema_json,
            "execution": "parallel_safe",
        }],
    }))
}

fn execute<T: AgentTool>(tool: &T, request: &Value) -> RequestOutcome {
    let Some(name) = request.get("name").and_then(Value::as_str) else {
        return RequestOutcome::DomainError(json!("invalid_arguments"));
    };
    let Some(arguments_json) = request.get("arguments_json").and_then(Value::as_str) else {
        return RequestOutcome::DomainError(json!("invalid_arguments"));
    };
    if name != T::NAME {
        return RequestOutcome::DomainError(json!("not_found"));
    }
    let Ok(arguments) = serde_json::from_str::<T::Arguments>(arguments_json) else {
        return RequestOutcome::DomainError(json!("invalid_arguments"));
    };
    match tool.execute(arguments) {
        Ok(content) => RequestOutcome::Success(json!({
            "content_type": "text",
            "content": content,
            "metadata_json": r#"{"provider":"lenso-portable"}"#,
        })),
        Err(error) => match serde_json::to_value(error) {
            Ok(error) => RequestOutcome::DomainError(error),
            Err(error) => RequestOutcome::Failure(error.to_string()),
        },
    }
}

/// Exports one request Plugin as Wasm Component or Process according to the
/// active Cargo target.
#[macro_export]
macro_rules! export_request_plugin {
    (
        $plugin:ty {
            capability_id: $capability_id:literal,
            descriptor_version: $descriptor_version:literal,
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
                    match <super::__LensoExportedPlugin as $crate::RequestPlugin>::invoke(
                        &<super::__LensoExportedPlugin as ::std::default::Default>::default(),
                        &capability,
                        &operation,
                        request,
                    ) {
                        $crate::RequestOutcome::Success(value) =>
                            $crate::__private::serde_json::to_string(&value)
                                .map_err(|error| error.to_string()),
                        $crate::RequestOutcome::DomainError(value) =>
                            ::std::result::Result::Err(
                                $crate::__private::serde_json::to_string(&value)
                                    .unwrap_or_else(|_| "\"execution_failed\"".to_owned()),
                            ),
                        $crate::RequestOutcome::Failure(detail) =>
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
            struct Component(super::__LensoExportedPlugin);

            impl $crate::__private::lenso_process_sdk::ProcessPlugin for Component {
                fn descriptor(&self) -> $crate::__private::serde_json::Value {
                    $crate::__private::serde_json::json!({
                        "abi": "lenso.json-request@1",
                        "capabilities": [{
                            "capability_id": $capability_id,
                            "descriptor_version": $descriptor_version,
                            "request_operations": [$first_request $(, $request)*],
                        }],
                    })
                }

                fn invoke(
                    &self,
                    capability: &str,
                    operation: &str,
                    request: $crate::__private::serde_json::Value,
                ) -> $crate::__private::lenso_process_sdk::ProcessOutcome {
                    match <super::__LensoExportedPlugin as $crate::RequestPlugin>::invoke(
                        &self.0,
                        capability,
                        operation,
                        request,
                    ) {
                        $crate::RequestOutcome::Success(value) =>
                            $crate::__private::lenso_process_sdk::ProcessOutcome::Success(value),
                        $crate::RequestOutcome::DomainError(value) =>
                            $crate::__private::lenso_process_sdk::ProcessOutcome::DomainError(value),
                        $crate::RequestOutcome::Failure(detail) =>
                            $crate::__private::lenso_process_sdk::ProcessOutcome::Failure(detail),
                    }
                }
            }

            pub fn serve() {
                $crate::__private::lenso_process_sdk::serve(&Component(
                    <super::__LensoExportedPlugin as ::std::default::Default>::default(),
                ))
                .expect("serve Lenso Process Plugin");
            }
        }

        #[cfg(not(target_arch = "wasm32"))]
        fn main() {
            __lenso_process_export::serve();
        }
    };
}

/// Exports one typed Agent Tool from the same source as Wasm or Process.
#[macro_export]
macro_rules! export_agent_tool {
    ($plugin:ty $(,)?) => {
        $crate::export_request_plugin! {
            $crate::AgentToolAdapter<$plugin> {
                capability_id: "lenso.agent.tool-provider@2",
                descriptor_version: "2.0.0",
                requests: ["catalog", "execute"],
            }
        }
    };
}

#[doc(hidden)]
pub mod __private {
    pub use serde_json;

    #[cfg(target_arch = "wasm32")]
    pub use crate::__wasm as wasm;
    #[cfg(target_arch = "wasm32")]
    pub use lenso_guest_sdk;
    #[cfg(not(target_arch = "wasm32"))]
    pub use lenso_process_sdk;
}
