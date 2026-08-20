//! Native Rust Execution Adapter for statically linked Module packages.

use std::{collections::BTreeMap, rc::Rc};

use lenso_app_plan::{ExecutionClassId, ResolvedAppPlan};
use lenso_kernel::{
    ModuleLifecycle, NativeExecutionAdapter, NativeRequestEndpoint, NoopModuleLifecycle,
    PreparedBinding, PreparedNativeApp, PreparedNativeModule, RuntimeFailure,
};

/// Endpoints created for one statically linked Module Instance generation.
#[derive(Debug)]
pub struct NativeModuleInstance {
    endpoints: Vec<Rc<dyn NativeRequestEndpoint>>,
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
            endpoints,
            lifecycle: Rc::new(lifecycle),
        }
    }

    /// Returns the lifecycle Interface for this generation.
    pub fn lifecycle(&self) -> Rc<dyn ModuleLifecycle> {
        self.lifecycle.clone()
    }

    /// Returns the exact endpoint set created for this generation.
    pub fn endpoints(&self) -> &[Rc<dyn NativeRequestEndpoint>] {
        &self.endpoints
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

impl NativeModuleRegistry {
    /// Creates an empty linked-factory registry.
    pub fn new() -> Self {
        Self::default()
    }
    /// Adds one statically linked factory.
    #[must_use]
    pub fn with_factory(mut self, factory: impl NativeModuleFactory) -> Self {
        self.factories.push(Rc::new(factory));
        self
    }
}

impl NativeExecutionAdapter for NativeModuleRegistry {
    fn prepare(&self, plan: &ResolvedAppPlan) -> Result<PreparedNativeApp, RuntimeFailure> {
        plan.validate()
            .map_err(|error| RuntimeFailure::InvalidResolvedPlan {
                detail: error.to_string(),
            })?;

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
                .filter(|factory| factory.package_id() == expected.package_id())
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
            let lifecycle = generation.lifecycle();
            generations.insert(
                expected.instance_key().to_owned(),
                PreparedNativeModule::with_lifecycle(
                    generation.endpoints.clone(),
                    lifecycle.clone(),
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

        let mut bindings = Vec::new();
        for binding in plan.capability_bindings() {
            if !instances.contains_key(binding.provider_instance()) {
                continue;
            }
            let endpoint = instances
                .get(binding.provider_instance())
                .and_then(|instance| {
                    instance.endpoints.iter().find(|endpoint| {
                        endpoint.capability_id() == binding.capability_id()
                            && endpoint.descriptor_version() == binding.descriptor_version()
                    })
                })
                .cloned()
                .ok_or_else(|| RuntimeFailure::InvalidResolvedPlan {
                    detail: format!(
                        "Capability `{}` version `{}` has no endpoint on provider `{}`",
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
        Ok(PreparedNativeApp::new(bindings, generations))
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
            .filter(|factory| factory.package_id() == expected.package_id())
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
        Ok(PreparedNativeModule::with_lifecycle(
            generation.endpoints.clone(),
            generation.lifecycle(),
        ))
    }
}

fn invalid<T>(detail: String) -> Result<T, RuntimeFailure> {
    Err(RuntimeFailure::InvalidResolvedPlan { detail })
}
