//! Native Rust Execution Adapter for statically linked Module packages.

use std::{collections::BTreeMap, rc::Rc};

use lenso_app_plan::ResolvedAppPlan;
use lenso_kernel::{
    ModuleLifecycle, NativeExecutionAdapter, NativeRequestEndpoint, NoopModuleLifecycle,
    PreparedNativeApp, RuntimeFailure,
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
    fn instantiate(&self, instance_key: &str) -> Result<NativeModuleInstance, RuntimeFailure>;
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
        let mut modules = BTreeMap::new();
        for expected in plan.module_instances() {
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
            let generation = factory.instantiate(expected.instance_key())?;
            validate_endpoint_set(
                expected.instance_key(),
                expected.provided_capabilities(),
                &generation.endpoints,
            )?;
            let lifecycle = generation.lifecycle();
            if instances
                .insert(expected.instance_key().to_owned(), generation)
                .is_some()
            {
                return invalid(format!(
                    "duplicate Module Instance `{}`",
                    expected.instance_key()
                ));
            }
            modules.insert(expected.instance_key().to_owned(), lifecycle);
        }

        let mut bindings = BTreeMap::new();
        for binding in plan.capability_bindings() {
            if !instances.contains_key(binding.consumer_instance()) {
                return invalid(format!(
                    "Capability `{}` names missing consumer `{}`",
                    binding.capability_id(),
                    binding.consumer_instance()
                ));
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
            bindings
                .entry((
                    binding.consumer_instance().to_owned(),
                    endpoint.capability_id(),
                ))
                .or_insert_with(Vec::new)
                .push(endpoint);
        }
        Ok(PreparedNativeApp::with_modules(bindings, modules))
    }
}

fn validate_endpoint_set(
    instance_key: &str,
    expected: &[lenso_app_plan::CapabilityEndpointPlan],
    actual: &[Rc<dyn NativeRequestEndpoint>],
) -> Result<(), RuntimeFailure> {
    if expected.len() != actual.len() {
        return invalid(format!(
            "Module Instance `{instance_key}` prepared {} endpoints; expected {}",
            actual.len(),
            expected.len()
        ));
    }
    for descriptor in expected {
        let matching: Vec<_> = actual
            .iter()
            .filter(|endpoint| endpoint.capability_id() == descriptor.capability_id())
            .collect();
        if matching.len() != 1 {
            return invalid(format!(
                "Module Instance `{instance_key}` prepared {} endpoints for Capability `{}`",
                matching.len(),
                descriptor.capability_id()
            ));
        }
        let endpoint = matching[0];
        let actual_operations: Vec<_> = endpoint.operations().to_vec();
        let expected_operations: Vec<_> =
            descriptor.operations().iter().map(String::as_str).collect();
        if endpoint.descriptor_version() != descriptor.descriptor_version()
            || actual_operations != expected_operations
        {
            return invalid(format!(
                "Module Instance `{instance_key}` endpoint `{}` differs from its resolved Descriptor",
                descriptor.capability_id()
            ));
        }
        let mut unique = actual_operations.clone();
        unique.sort_unstable();
        unique.dedup();
        if unique.len() != actual_operations.len() {
            return invalid(format!(
                "Module Instance `{instance_key}` endpoint `{}` has duplicate Operations",
                descriptor.capability_id()
            ));
        }
    }
    Ok(())
}

fn invalid<T>(detail: String) -> Result<T, RuntimeFailure> {
    Err(RuntimeFailure::InvalidResolvedPlan { detail })
}
