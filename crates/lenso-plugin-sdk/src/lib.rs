//! Execution-target lowering for portable Rust Plugins.
//!
//! Product SDKs own Capability semantics and typed authoring. This crate owns
//! only the final JSON request seam and lowers it to Wasm Component or Process
//! transport glue. Plugin authors normally use a product SDK instead of this
//! interface directly.

use serde_json::Value;

pub use lenso_plugin_sdk_macros::plugin;

/// One source-declared Capability dependency of a portable Plugin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Requirement {
    pub requirement_id: &'static str,
    pub capability_id: &'static str,
    pub descriptor_version: &'static str,
    pub descriptor_digest: &'static str,
    pub cardinality: Cardinality,
}

impl Requirement {
    /// Declares exactly one provider for a typed dependency role.
    pub const fn one<C: DependencyClient>(requirement_id: &'static str) -> Self {
        Self::new::<C>(requirement_id, Cardinality::One)
    }

    /// Declares zero or one provider for a typed dependency role.
    pub const fn optional<C: DependencyClient>(requirement_id: &'static str) -> Self {
        Self::new::<C>(requirement_id, Cardinality::Optional)
    }

    /// Declares any number of providers for a typed dependency role.
    pub const fn many<C: DependencyClient>(requirement_id: &'static str) -> Self {
        Self::new::<C>(requirement_id, Cardinality::Many)
    }

    const fn new<C: DependencyClient>(
        requirement_id: &'static str,
        cardinality: Cardinality,
    ) -> Self {
        Self {
            requirement_id,
            capability_id: C::CAPABILITY_ID,
            descriptor_version: C::DESCRIPTOR_VERSION,
            descriptor_digest: C::DESCRIPTOR_DIGEST,
            cardinality,
        }
    }
}

/// Supported dependency cardinalities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cardinality {
    One,
    Optional,
    Many,
}

/// One exact Host-selected provider route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dependency {
    requirement_id: String,
    route_id: String,
    provider_instance: String,
    capability_id: String,
    descriptor_version: String,
    descriptor_digest: String,
}

impl Dependency {
    /// Stable consumer-local role declared by the Plugin.
    pub fn requirement_id(&self) -> &str {
        &self.requirement_id
    }

    /// Exact provider instance selected by the immutable Plan.
    pub fn provider_instance(&self) -> &str {
        &self.provider_instance
    }

    /// Converts this exact route into its generated typed client.
    pub fn client<C: DependencyClient>(self) -> Result<C, String> {
        if self.capability_id != C::CAPABILITY_ID
            || self.descriptor_version != C::DESCRIPTOR_VERSION
            || self.descriptor_digest != C::DESCRIPTOR_DIGEST
        {
            return Err(format!(
                "dependency `{}` does not match generated client `{}`",
                self.requirement_id,
                C::CAPABILITY_ID
            ));
        }
        Ok(C::from_dependency(self))
    }
}

/// Contract implemented by generated portable Capability clients.
pub trait DependencyClient: Sized {
    const CAPABILITY_ID: &'static str;
    const DESCRIPTOR_VERSION: &'static str;
    const DESCRIPTOR_DIGEST: &'static str;

    #[doc(hidden)]
    fn from_dependency(dependency: Dependency) -> Self;
}

/// Exact dependencies and configuration supplied while constructing a Plugin.
#[derive(Clone, Debug)]
pub struct CreateContext {
    config: Value,
    dependencies: Dependencies,
    call: Ctx,
}

impl CreateContext {
    /// Resolved configuration for this Plugin instance.
    pub const fn config(&self) -> &Value {
        &self.config
    }

    /// Exact named dependency routes selected by the Plan.
    pub const fn dependencies(&self) -> &Dependencies {
        &self.dependencies
    }

    /// Lifecycle call context. Constructor dependency calls inherit this scope.
    pub const fn ctx(&self) -> &Ctx {
        &self.call
    }
}

/// Exact dependency routes selected for one Plugin generation.
#[derive(Clone, Debug, Default)]
pub struct Dependencies {
    routes: Vec<Dependency>,
}

impl Dependencies {
    /// Resolves exactly one provider for a named dependency role.
    pub fn one(&self, requirement_id: &str) -> Result<Dependency, String> {
        let matches = self
            .routes
            .iter()
            .filter(|route| route.requirement_id == requirement_id)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [route] => Ok((*route).clone()),
            [] => Err(format!("dependency `{requirement_id}` has no provider")),
            _ => Err(format!(
                "dependency `{requirement_id}` has multiple providers"
            )),
        }
    }

    /// Resolves zero or one provider for a named dependency role.
    pub fn optional(&self, requirement_id: &str) -> Result<Option<Dependency>, String> {
        let matches = self
            .routes
            .iter()
            .filter(|route| route.requirement_id == requirement_id)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [] => Ok(None),
            [route] => Ok(Some((*route).clone())),
            _ => Err(format!(
                "dependency `{requirement_id}` has multiple providers"
            )),
        }
    }

    /// Resolves all providers for a named dependency role in Plan order.
    pub fn many(&self, requirement_id: &str) -> Vec<Dependency> {
        self.routes
            .iter()
            .filter(|route| route.requirement_id == requirement_id)
            .cloned()
            .collect()
    }
}

/// Domain, Runtime, and value failures from one portable dependency call.
#[derive(Clone, Debug, PartialEq)]
pub enum CallError<E> {
    Domain(E),
    Runtime(Value),
    InvalidValue,
}

/// Current invocation or lifecycle authority supplied by the Runtime.
#[derive(Clone, Debug)]
pub struct Ctx {
    #[cfg(not(target_arch = "wasm32"))]
    inner: lenso_process_sdk::ProcessCallContext,
}

impl Ctx {
    /// Whether cooperative cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner.is_cancelled()
        }
        #[cfg(target_arch = "wasm32")]
        {
            false
        }
    }

    /// Calls one exact dependency route with generated request/response types.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn request<Request, Response, DomainError>(
        &self,
        dependency: &Dependency,
        operation: &str,
        request: &Request,
    ) -> Result<Response, CallError<DomainError>>
    where
        Request: serde::Serialize,
        Response: serde::de::DeserializeOwned,
        DomainError: serde::de::DeserializeOwned,
    {
        let payload = serde_json::to_value(request).map_err(|_| CallError::InvalidValue)?;
        match self
            .inner
            .call(
                dependency.requirement_id.clone(),
                dependency.route_id.clone(),
                operation,
                payload,
            )
            .map_err(|detail| CallError::Runtime(serde_json::json!({ "detail": detail })))?
        {
            lenso_process_sdk::authoring::InvocationOutcome::Success { value } => {
                serde_json::from_value(value).map_err(|_| CallError::InvalidValue)
            }
            lenso_process_sdk::authoring::InvocationOutcome::Domain { error } => {
                match serde_json::from_value(error) {
                    Ok(error) => Err(CallError::Domain(error)),
                    Err(_) => Err(CallError::InvalidValue),
                }
            }
            lenso_process_sdk::authoring::InvocationOutcome::Runtime { failure } => Err(
                CallError::Runtime(serde_json::to_value(failure).unwrap_or(Value::Null)),
            ),
        }
    }
}

/// Complete portable Plugin object constructed once per admitted generation.
pub trait Plugin: Send + Sync + 'static {
    /// Source-declared named dependencies required by this object.
    fn requirements() -> &'static [Requirement] {
        &[]
    }

    /// Constructs the complete object. Dependency calls use `context.ctx()`.
    fn create(context: CreateContext) -> Result<Self, String>
    where
        Self: Sized;

    /// Attempts cleanup once after admitted work has drained.
    fn stop(&self, _context: Ctx) -> Result<(), String> {
        Ok(())
    }
}

impl<T> Plugin for T
where
    T: Default + Send + Sync + 'static,
{
    fn create(_context: CreateContext) -> Result<Self, String> {
        Ok(Self::default())
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub fn __process_descriptor(base: &str, requirements: &[Requirement]) -> String {
    let mut value: Value = serde_json::from_str(base).expect("generated descriptor is valid JSON");
    let object = value
        .as_object_mut()
        .expect("generated descriptor is a JSON object");
    if requirements.is_empty() {
        return base.to_owned();
    }
    object.insert(
        "abi".to_owned(),
        Value::String("lenso.json-host-imports@2".to_owned()),
    );
    let mut requirements = requirements
        .iter()
        .map(|requirement| {
            serde_json::json!({
                "requirement_id": requirement.requirement_id,
                "capability_id": requirement.capability_id,
                "descriptor_version": requirement.descriptor_version,
                "cardinality": match requirement.cardinality {
                    Cardinality::One => "one",
                    Cardinality::Optional => "optional",
                    Cardinality::Many => "many",
                },
            })
        })
        .collect::<Vec<_>>();
    requirements.sort_by(|left, right| {
        left["requirement_id"]
            .as_str()
            .cmp(&right["requirement_id"].as_str())
    });
    object.insert(
        "required_capabilities".to_owned(),
        Value::Array(requirements),
    );
    serde_json::to_string(&value).expect("generated descriptor remains valid JSON")
}

#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub fn __validate_process_initialization<P: Plugin>(
    params: &lenso_process_sdk::authoring::InitializeParams,
) -> Result<(), String> {
    use lenso_process_sdk::authoring::RequirementCardinality;

    let declared = params
        .required_declarations
        .iter()
        .map(|requirement| {
            (
                requirement.requirement_id.as_str(),
                requirement.capability_id.as_str(),
                requirement.descriptor_version.as_str(),
                requirement.descriptor_digest.as_str(),
                requirement.cardinality,
            )
        })
        .collect::<Vec<_>>();
    let expected = P::requirements()
        .iter()
        .map(|requirement| {
            (
                requirement.requirement_id,
                requirement.capability_id,
                requirement.descriptor_version,
                requirement.descriptor_digest,
                match requirement.cardinality {
                    Cardinality::One => RequirementCardinality::One,
                    Cardinality::Optional => RequirementCardinality::Optional,
                    Cardinality::Many => RequirementCardinality::Many,
                },
            )
        })
        .collect::<Vec<_>>();
    if declared == expected {
        Ok(())
    } else {
        Err("Host initialization does not match source-declared dependencies".to_owned())
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub fn __process_create_context(
    initialization: &lenso_process_sdk::authoring::InitializeParams,
    call: lenso_process_sdk::ProcessLifecycleContext,
) -> CreateContext {
    let routes = initialization
        .routes
        .iter()
        .map(|route| Dependency {
            requirement_id: route.requirement_id.clone(),
            route_id: route.route_id.clone(),
            provider_instance: route.provider_instance.clone(),
            capability_id: route.capability_id.clone(),
            descriptor_version: route.descriptor_version.clone(),
            descriptor_digest: route.descriptor_digest.clone(),
        })
        .collect();
    CreateContext {
        config: initialization.config.clone(),
        dependencies: Dependencies { routes },
        call: Ctx { inner: call },
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub fn __process_ctx(call: lenso_process_sdk::ProcessCallContext) -> Ctx {
    Ctx { inner: call }
}

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
pub trait JsonRequestHandler {
    /// Handles one validated Capability request.
    fn invoke(&self, _capability: &str, _operation: &str, _request: Value) -> InvocationOutcome {
        InvocationOutcome::Failure("this Plugin requires invocation context".to_owned())
    }

    /// Handles one request with its exact invocation authority.
    fn invoke_with_context(
        &self,
        _context: Ctx,
        capability: &str,
        operation: &str,
        request: Value,
    ) -> InvocationOutcome {
        self.invoke(capability, operation, request)
    }
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
            #[derive(Default)]
            struct Component {
                initialization: ::std::sync::Mutex<::std::option::Option<
                    $crate::__private::lenso_process_sdk::authoring::InitializeParams,
                >>,
            }

            const DESCRIPTOR: &str =
                $crate::__private::lenso_guest_sdk::__request_plugin_descriptor!(
                    $capability_id,
                    $descriptor_version,
                    digest: $descriptor_digest,
                    $first_request $(, $request)*
                );

            impl $crate::__private::lenso_process_sdk::ProcessPluginV2 for Component {
                type Instance = super::__LensoExportedPlugin;

                fn initialize(
                    &self,
                    params: &$crate::__private::lenso_process_sdk::authoring::InitializeParams,
                ) -> ::std::result::Result<(), ::std::string::String> {
                    $crate::__validate_process_initialization::<super::__LensoExportedPlugin>(params)?;
                    let mut initialization = self
                        .initialization
                        .lock()
                        .map_err(|_| "Plugin initialization state was poisoned".to_owned())?;
                    if initialization.replace(params.clone()).is_some() {
                        return Err("Plugin initialized more than once".to_owned());
                    }
                    Ok(())
                }

                fn construct(
                    &self,
                    _params: &$crate::__private::lenso_process_sdk::authoring::ConstructParams,
                    context: $crate::__private::lenso_process_sdk::ProcessLifecycleContext,
                ) -> ::std::result::Result<Self::Instance, ::std::string::String> {
                    let initialization = self
                        .initialization
                        .lock()
                        .map_err(|_| "Plugin initialization state was poisoned".to_owned())?
                        .clone()
                        .ok_or_else(|| "Plugin constructed before initialization".to_owned())?;
                    <super::__LensoExportedPlugin as $crate::Plugin>::create(
                        $crate::__process_create_context(&initialization, context),
                    )
                }

                fn invoke(
                    &self,
                    instance: &Self::Instance,
                    params: $crate::__private::lenso_process_sdk::authoring::InvokeParams,
                    context: $crate::__private::lenso_process_sdk::ProcessInvocationContext,
                ) -> $crate::__private::lenso_process_sdk::authoring::InvocationOutcome {
                    match <super::__LensoExportedPlugin as $crate::JsonRequestHandler>::invoke_with_context(
                        instance,
                        $crate::__process_ctx(context),
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

                fn stop(
                    &self,
                    instance: &Self::Instance,
                    _params: &$crate::__private::lenso_process_sdk::authoring::StopParams,
                    context: $crate::__private::lenso_process_sdk::ProcessLifecycleContext,
                ) -> $crate::__private::lenso_process_sdk::ProcessStopOutcome {
                    match <super::__LensoExportedPlugin as $crate::Plugin>::stop(
                        instance,
                        $crate::__process_ctx(context),
                    ) {
                        Ok(()) => $crate::__private::lenso_process_sdk::ProcessStopOutcome::Completed,
                        Err(detail) => $crate::__private::lenso_process_sdk::ProcessStopOutcome::Failed(detail),
                    }
                }
            }

            pub fn serve() {
                if ::std::env::args_os().nth(1).as_deref()
                    == Some(::std::ffi::OsStr::new("--lenso-describe"))
                {
                    println!(
                        "{}",
                        $crate::__process_descriptor(
                            DESCRIPTOR,
                            <super::__LensoExportedPlugin as $crate::Plugin>::requirements(),
                        ),
                    );
                    return;
                }
                $crate::__private::lenso_process_sdk::serve_v2(Component::default())
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use lenso_process_sdk::authoring::{
        AuthoringLimits, InitializeParams, ProvidedEndpoint, RequirementCardinality,
        RequirementDeclaration, RouteDescriptor, SessionIdentity,
    };

    const STORE_DIGEST: &str =
        "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    const REQUIREMENTS: &[Requirement] = &[
        Requirement {
            requirement_id: "destination",
            capability_id: "example.store@1",
            descriptor_version: "1.0.0",
            descriptor_digest: STORE_DIGEST,
            cardinality: Cardinality::One,
        },
        Requirement {
            requirement_id: "source",
            capability_id: "example.store@1",
            descriptor_version: "1.0.0",
            descriptor_digest: STORE_DIGEST,
            cardinality: Cardinality::One,
        },
    ];

    struct Stateful;

    impl Plugin for Stateful {
        fn requirements() -> &'static [Requirement] {
            REQUIREMENTS
        }

        fn create(_context: CreateContext) -> Result<Self, String> {
            Ok(Self)
        }
    }

    fn initialization() -> InitializeParams {
        InitializeParams {
            api_version: 2,
            identity: SessionIdentity {
                session: "session-1".to_owned(),
                plugin_instance: "sync".to_owned(),
                plugin_generation: "generation-1".to_owned(),
                artifact_digest: STORE_DIGEST.to_owned(),
                contract_digest: STORE_DIGEST.to_owned(),
                runtime_profile: "lenso.process-stdio@2".to_owned(),
                value_profile: "lenso-json-value-v1".to_owned(),
            },
            config: serde_json::json!({ "prefix": "copied" }),
            required_declarations: REQUIREMENTS
                .iter()
                .map(|requirement| RequirementDeclaration {
                    requirement_id: requirement.requirement_id.to_owned(),
                    capability_id: requirement.capability_id.to_owned(),
                    descriptor_version: requirement.descriptor_version.to_owned(),
                    descriptor_digest: requirement.descriptor_digest.to_owned(),
                    cardinality: RequirementCardinality::One,
                })
                .collect(),
            routes: REQUIREMENTS
                .iter()
                .enumerate()
                .map(|(index, requirement)| RouteDescriptor {
                    route_id: format!("route-{index}"),
                    requirement_id: requirement.requirement_id.to_owned(),
                    capability_id: requirement.capability_id.to_owned(),
                    descriptor_version: requirement.descriptor_version.to_owned(),
                    descriptor_digest: requirement.descriptor_digest.to_owned(),
                    provider_instance: format!("store-{index}"),
                    provider_order: 0,
                })
                .collect(),
            provided_endpoints: vec![ProvidedEndpoint {
                endpoint_id: "sync".to_owned(),
                capability_id: "example.sync@1".to_owned(),
                descriptor_version: "1.0.0".to_owned(),
                descriptor_digest: STORE_DIGEST.to_owned(),
            }],
            limits: AuthoringLimits::defaults(),
        }
    }

    #[test]
    fn process_descriptor_contains_sorted_named_requirements() {
        let base = r#"{"abi":"lenso.json-request@1","capabilities":[{"capability_id":"example.sync@1","descriptor_version":"1.0.0","request_operations":["sync"]}]}"#;
        let descriptor: Value =
            serde_json::from_str(&__process_descriptor(base, REQUIREMENTS)).unwrap();
        assert_eq!(descriptor["abi"], "lenso.json-host-imports@2");
        assert_eq!(
            descriptor["required_capabilities"][0]["requirement_id"],
            "destination"
        );
        assert_eq!(
            descriptor["required_capabilities"][1]["requirement_id"],
            "source"
        );
    }

    #[test]
    fn process_initialization_must_match_source_declarations_exactly() {
        let initialization = initialization();
        assert!(__validate_process_initialization::<Stateful>(&initialization).is_ok());

        let mut drifted = initialization;
        drifted.required_declarations[0].descriptor_digest = STORE_DIGEST.replace('1', "2");
        assert_eq!(
            __validate_process_initialization::<Stateful>(&drifted).unwrap_err(),
            "Host initialization does not match source-declared dependencies"
        );
    }
}
