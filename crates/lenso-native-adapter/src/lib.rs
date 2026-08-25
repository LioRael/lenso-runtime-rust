//! Native Rust Execution Adapter for statically linked Module packages.

use std::{collections::BTreeMap, rc::Rc};

#[doc(hidden)]
pub use inventory as __inventory;
use lenso_app_plan::{ExecutionClassId, ResolvedAppPlan};
pub use lenso_kernel::RuntimeFailure;
pub use lenso_native_adapter_macros::{ModuleConfig, module, provides};

/// Implementation details referenced by generated Module glue.
#[doc(hidden)]
pub mod __private {
    pub use crate::{
        __inventory, LinkedNativeModuleFactory, NativeModuleFactory, NativeModuleFactoryContext,
        NativeModuleInstance, RuntimeFailure,
    };
    pub use futures;
    pub use lenso_kernel::{
        ActivateContext, DeactivateContext, ModuleFuture, ModuleLifecycle, NativeEventEndpoint,
        NativeRequestEndpoint, NativeStreamEndpoint, PrepareContext,
    };
    pub use serde_json;
}

use lenso_kernel::{
    ModuleLifecycle, NativeEndpointSet, NativeEventEndpoint, NativeExecutionAdapter,
    NativeRequestEndpoint, NativeStreamEndpoint, NoopModuleLifecycle, PreparedBinding,
    PreparedEventBinding, PreparedNativeApp, PreparedNativeModule, PreparedStreamBinding,
};

/// One native Module factory contributed to the Host's link-time catalog.
#[derive(Clone, Copy, Debug)]
#[doc(hidden)]
pub struct LinkedNativeModuleFactory {
    constructor: fn() -> Rc<dyn NativeModuleFactory>,
}

impl LinkedNativeModuleFactory {
    /// Creates a link-time catalog record. Intended for generated authoring glue.
    #[doc(hidden)]
    pub const fn new(constructor: fn() -> Rc<dyn NativeModuleFactory>) -> Self {
        Self { constructor }
    }
}

inventory::collect!(LinkedNativeModuleFactory);

/// Endpoints created for one statically linked Module Instance generation.
#[derive(Debug)]
pub struct NativeModuleInstance {
    endpoints: NativeEndpointSet,
    lifecycle: Rc<dyn ModuleLifecycle>,
}

impl NativeModuleInstance {
    /// Creates a generation from its exact declared endpoint set.
    pub fn new(endpoints: Vec<Rc<dyn NativeRequestEndpoint>>) -> Self {
        Self::with_lifecycle(endpoints, NoopModuleLifecycle)
    }

    /// Creates a generation with its exact endpoints and lifecycle Interface.
    pub fn with_lifecycle(
        endpoints: Vec<Rc<dyn NativeRequestEndpoint>>,
        lifecycle: impl ModuleLifecycle,
    ) -> Self {
        Self {
            endpoints: NativeEndpointSet::new(endpoints, Vec::new(), Vec::new()),
            lifecycle: Rc::new(lifecycle),
        }
    }

    /// Creates a generation with request and bidirectional stream endpoints.
    pub fn with_endpoints(
        endpoints: Vec<Rc<dyn NativeRequestEndpoint>>,
        stream_endpoints: Vec<Rc<dyn NativeStreamEndpoint>>,
        lifecycle: impl ModuleLifecycle,
    ) -> Self {
        Self {
            endpoints: NativeEndpointSet::new(endpoints, stream_endpoints, Vec::new()),
            lifecycle: Rc::new(lifecycle),
        }
    }

    /// Creates a generation containing only bidirectional stream endpoints.
    pub fn with_stream_endpoints(
        stream_endpoints: Vec<Rc<dyn NativeStreamEndpoint>>,
        lifecycle: impl ModuleLifecycle,
    ) -> Self {
        Self::with_endpoints(Vec::new(), stream_endpoints, lifecycle)
    }

    /// Creates a generation containing only ephemeral Event endpoints.
    pub fn with_event_endpoints(
        event_endpoints: Vec<Rc<dyn NativeEventEndpoint>>,
        lifecycle: impl ModuleLifecycle,
    ) -> Self {
        Self {
            endpoints: NativeEndpointSet::new(Vec::new(), Vec::new(), event_endpoints),
            lifecycle: Rc::new(lifecycle),
        }
    }

    /// Creates a generation with request, stream, and ephemeral Event endpoints.
    pub fn with_all_endpoints(
        endpoints: Vec<Rc<dyn NativeRequestEndpoint>>,
        stream_endpoints: Vec<Rc<dyn NativeStreamEndpoint>>,
        event_endpoints: Vec<Rc<dyn NativeEventEndpoint>>,
        lifecycle: impl ModuleLifecycle,
    ) -> Self {
        Self {
            endpoints: NativeEndpointSet::new(endpoints, stream_endpoints, event_endpoints),
            lifecycle: Rc::new(lifecycle),
        }
    }

    /// Returns the lifecycle Interface for this generation.
    pub fn lifecycle(&self) -> Rc<dyn ModuleLifecycle> {
        self.lifecycle.clone()
    }

    /// Returns the exact endpoint set created for this generation.
    pub fn endpoints(&self) -> &[Rc<dyn NativeRequestEndpoint>] {
        self.endpoints.request()
    }

    /// Returns the exact bidirectional stream endpoint set created for this generation.
    pub fn stream_endpoints(&self) -> &[Rc<dyn NativeStreamEndpoint>] {
        self.endpoints.stream()
    }

    /// Returns the exact ephemeral Event endpoint set created for this Instance.
    pub fn event_endpoints(&self) -> &[Rc<dyn NativeEventEndpoint>] {
        self.endpoints.event()
    }
}

impl Default for NativeModuleInstance {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

/// Adapter-specific factory for a statically linked native Rust Module.
pub trait NativeModuleFactory: std::fmt::Debug + 'static {
    /// Package identity selected by the Resolved App Plan.
    fn package_id(&self) -> &'static str;
    /// Exact statically linked Cargo package version.
    fn package_version(&self) -> &'static str {
        ""
    }
    /// Immutable factory identity advertised by the exact Host Build Manifest.
    ///
    /// Plugin-resolved Plans carry this value as their package revision. The
    /// default keeps ordinary statically linked factories unique by package and
    /// version while allowing a factory to override the identity when its build
    /// authority is more specific than a Cargo package version.
    fn factory_identity(&self) -> String {
        let version = self.package_version();
        if version.is_empty() {
            self.package_id().to_owned()
        } else {
            format!("{}@{version}", self.package_id())
        }
    }
    /// Creates a fresh Module Instance generation.
    fn instantiate(
        &self,
        context: NativeModuleFactoryContext<'_>,
    ) -> Result<NativeModuleInstance, RuntimeFailure>;
}

/// Immutable Plan input supplied when a native factory creates one generation.
#[derive(Clone, Copy, Debug)]
pub struct NativeModuleFactoryContext<'a> {
    instance_key: &'a str,
    entrypoint: &'a str,
    configuration: &'a str,
}

impl<'a> NativeModuleFactoryContext<'a> {
    fn from_plan(instance: &'a lenso_app_plan::ModuleInstancePlan) -> Self {
        Self {
            instance_key: instance.instance_key(),
            entrypoint: instance.entrypoint(),
            configuration: instance.configuration(),
        }
    }

    /// Returns the App-local Module Instance key.
    pub const fn instance_key(self) -> &'a str {
        self.instance_key
    }

    /// Returns the exact package entrypoint selected before boot.
    pub const fn entrypoint(self) -> &'a str {
        self.entrypoint
    }

    /// Returns opaque Module-owned configuration selected before boot.
    pub const fn configuration(self) -> &'a str {
        self.configuration
    }
}

/// Statically linked native Module factories available to an App binary.
#[derive(Debug, Default)]
pub struct NativeModuleRegistry {
    factories: Vec<Rc<dyn NativeModuleFactory>>,
}

type NativeInstances = BTreeMap<String, NativeModuleInstance>;
type PreparedGenerations = BTreeMap<String, PreparedNativeModule>;
type NativeBindings = (
    Vec<PreparedBinding>,
    Vec<PreparedStreamBinding>,
    Vec<PreparedEventBinding>,
);

fn factory_matches(
    factory: &dyn NativeModuleFactory,
    expected: &lenso_app_plan::ModuleInstancePlan,
) -> bool {
    factory.package_id() == expected.package_id()
        && (expected.package_revision().is_empty()
            || factory.package_version() == expected.package_revision()
            || factory.factory_identity() == expected.package_revision())
}

impl NativeModuleRegistry {
    /// Creates an empty linked-factory registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds every Module factory contributed to this Host at link time.
    ///
    /// This catalog describes code available in the binary. The Resolved App
    /// Plan remains the sole authority that selects and binds Module Instances.
    #[must_use]
    pub fn with_linked_factories(mut self) -> Self {
        self.factories.extend(
            inventory::iter::<LinkedNativeModuleFactory>
                .into_iter()
                .map(|linked| (linked.constructor)()),
        );
        self.factories
            .sort_by_key(|factory| factory.factory_identity());
        self
    }

    /// Returns the exact native factories available to this registry.
    pub fn factories(&self) -> impl Iterator<Item = &dyn NativeModuleFactory> {
        self.factories.iter().map(std::convert::AsRef::as_ref)
    }
    /// Adds one statically linked factory.
    #[must_use]
    pub fn with_factory(mut self, factory: impl NativeModuleFactory) -> Self {
        self.factories.push(Rc::new(factory));
        self
    }

    fn prepare_instances(
        &self,
        plan: &ResolvedAppPlan,
    ) -> Result<(NativeInstances, PreparedGenerations), RuntimeFailure> {
        let mut instances = BTreeMap::new();
        let mut generations = BTreeMap::new();
        for expected in plan
            .module_instances()
            .iter()
            .filter(|instance| instance.execution_class() == &ExecutionClassId::native_rust())
        {
            let matching_factories: Vec<_> = self
                .factories
                .iter()
                .filter(|factory| factory_matches(factory.as_ref(), expected))
                .collect();
            let factory = match matching_factories.as_slice() {
                [] => {
                    return Err(RuntimeFailure::MissingModuleFactory {
                        instance: expected.instance_key().to_owned(),
                        package_id: expected.package_id().to_owned(),
                    });
                }
                [factory] => *factory,
                _ => {
                    return invalid(format!(
                        "multiple statically linked factories declare package `{}`",
                        expected.package_id()
                    ));
                }
            };
            let generation =
                factory.instantiate(NativeModuleFactoryContext::from_plan(expected))?;
            generations.insert(
                expected.instance_key().to_owned(),
                PreparedNativeModule::with_endpoint_set_lifecycle(
                    generation.endpoints.clone(),
                    generation.lifecycle(),
                ),
            );
            if instances
                .insert(expected.instance_key().to_owned(), generation)
                .is_some()
            {
                return invalid(format!(
                    "duplicate Module Instance `{}`",
                    expected.instance_key()
                ));
            }
        }
        Ok((instances, generations))
    }
}

impl NativeExecutionAdapter for NativeModuleRegistry {
    fn prepare(&self, plan: &ResolvedAppPlan) -> Result<PreparedNativeApp, RuntimeFailure> {
        plan.validate()
            .map_err(|error| RuntimeFailure::InvalidResolvedPlan {
                detail: error.to_string(),
            })?;

        let (instances, generations) = self.prepare_instances(plan)?;
        let (bindings, stream_bindings, event_bindings) = prepare_bindings(plan, &instances)?;
        Ok(PreparedNativeApp::new(bindings, generations)
            .with_stream_bindings(stream_bindings)
            .with_event_bindings(event_bindings))
    }

    fn recreate(
        &self,
        plan: &ResolvedAppPlan,
        instance_key: &str,
    ) -> Result<PreparedNativeModule, RuntimeFailure> {
        let expected = plan
            .module_instances()
            .iter()
            .find(|instance| instance.instance_key() == instance_key)
            .ok_or_else(|| RuntimeFailure::InvalidResolvedPlan {
                detail: format!("unknown Module Instance `{instance_key}`"),
            })?;
        let matching_factories: Vec<_> = self
            .factories
            .iter()
            .filter(|factory| factory_matches(factory.as_ref(), expected))
            .collect();
        let factory = match matching_factories.as_slice() {
            [] => {
                return Err(RuntimeFailure::MissingModuleFactory {
                    instance: expected.instance_key().to_owned(),
                    package_id: expected.package_id().to_owned(),
                });
            }
            [factory] => *factory,
            _ => {
                return invalid(format!(
                    "multiple statically linked factories declare package `{}`",
                    expected.package_id()
                ));
            }
        };
        let generation = factory.instantiate(NativeModuleFactoryContext::from_plan(expected))?;
        Ok(PreparedNativeModule::with_endpoint_set_lifecycle(
            generation.endpoints.clone(),
            generation.lifecycle(),
        ))
    }
}

fn prepare_bindings(
    plan: &ResolvedAppPlan,
    instances: &NativeInstances,
) -> Result<NativeBindings, RuntimeFailure> {
    let mut bindings = Vec::new();
    let mut stream_bindings = Vec::new();
    let mut event_bindings = Vec::new();
    for binding in plan.capability_bindings() {
        if !instances.contains_key(binding.provider_instance()) {
            continue;
        }
        let provider = plan
            .module_instance(binding.provider_instance())
            .expect("validated binding provider should exist");
        let descriptor = provider
            .provided_capabilities()
            .iter()
            .find(|descriptor| descriptor.capability_id() == binding.capability_id())
            .expect("validated binding descriptor should exist");
        if !descriptor.request_operations().is_empty() {
            let endpoint = instances
                .get(binding.provider_instance())
                .and_then(|instance| {
                    instance.endpoints.request().iter().find(|endpoint| {
                        endpoint.capability_id() == binding.capability_id()
                            && endpoint.descriptor_version() == binding.descriptor_version()
                    })
                })
                .cloned()
                .ok_or_else(|| RuntimeFailure::InvalidResolvedPlan {
                    detail: format!(
                        "Capability `{}` version `{}` has no request endpoint on provider `{}`",
                        binding.capability_id(),
                        binding.descriptor_version(),
                        binding.provider_instance()
                    ),
                })?;
            bindings.push(PreparedBinding::new(
                binding.consumer_instance(),
                binding.provider_instance(),
                endpoint,
            ));
        }
        if !descriptor.stream_operations().is_empty() {
            let endpoint = instances
                .get(binding.provider_instance())
                .and_then(|instance| {
                    instance.endpoints.stream().iter().find(|endpoint| {
                        endpoint.capability_id() == binding.capability_id()
                            && endpoint.descriptor_version() == binding.descriptor_version()
                    })
                })
                .cloned()
                .ok_or_else(|| RuntimeFailure::InvalidResolvedPlan {
                    detail: format!(
                        "Capability `{}` version `{}` has no stream endpoint on provider `{}`",
                        binding.capability_id(),
                        binding.descriptor_version(),
                        binding.provider_instance()
                    ),
                })?;
            stream_bindings.push(PreparedStreamBinding::new(
                binding.consumer_instance(),
                binding.provider_instance(),
                endpoint,
            ));
        }
        if !descriptor.event_operations().is_empty() {
            let endpoint = instances
                .get(binding.provider_instance())
                .and_then(|instance| {
                    instance.endpoints.event().iter().find(|endpoint| {
                        endpoint.capability_id() == binding.capability_id()
                            && endpoint.descriptor_version() == binding.descriptor_version()
                    })
                })
                .cloned()
                .ok_or_else(|| RuntimeFailure::InvalidResolvedPlan {
                    detail: format!(
                        "Capability `{}` version `{}` has no Event endpoint on provider `{}`",
                        binding.capability_id(),
                        binding.descriptor_version(),
                        binding.provider_instance()
                    ),
                })?;
            event_bindings.push(PreparedEventBinding::new(
                binding.consumer_instance(),
                binding.provider_instance(),
                endpoint,
            ));
        }
    }
    Ok((bindings, stream_bindings, event_bindings))
}

fn invalid<T>(detail: String) -> Result<T, RuntimeFailure> {
    Err(RuntimeFailure::InvalidResolvedPlan { detail })
}
