//! Native Rust Execution Adapter for statically linked Plugin packages.

mod authoring;
mod managed_tasks;

use std::{
    collections::BTreeMap,
    rc::Rc,
    sync::{Mutex, OnceLock},
};

#[doc(hidden)]
pub use authoring::{CompleteObjectLifecycle, ConstructionContext, LifecycleContext, PluginObject};
#[doc(hidden)]
pub use inventory as __inventory;
use lenso_app_plan::{
    ExecutionClassId, ResolvedAppPlan,
    authoring::{HostCatalog, HostDefaultPlugin, HostPluginRelease, HostSlot, PluginDescriptor},
};
use lenso_kernel::{ActivateContext, DeactivateContext, PrepareContext};
pub use lenso_kernel::{CancellationToken, RuntimeFailure};
pub use lenso_native_adapter_macros::{PluginConfig, plugin, plugin_impl, provides};
pub use lenso_runtime_codec::InstanceResources;
pub use managed_tasks::{ManagedTasks, ManagedTasksError};

/// Optional convention-based lifecycle hooks for a struct-level Plugin.
///
/// Add `#[plugin(lifecycle)]`, implement this trait, and override only the
/// phases that own real work. The generated Adapter lifecycle still connects
/// declared Capability ports before `activate`.
#[allow(async_fn_in_trait)]
pub trait Lifecycle: Clone + 'static {
    async fn prepare(&self, _context: PrepareContext) -> Result<(), RuntimeFailure> {
        Ok(())
    }

    async fn activate(&self, _context: ActivateContext) -> Result<(), RuntimeFailure> {
        Ok(())
    }

    async fn deactivate(&self, _context: DeactivateContext) -> Result<(), RuntimeFailure> {
        Ok(())
    }
}

/// Implementation details referenced by generated Plugin glue.
#[doc(hidden)]
pub mod __private {
    pub use crate::authoring::{ErasedConstructionFuture, LinkedPluginConstruction};
    pub use crate::{
        __inventory, CompleteObjectLifecycle, ConstructionContext, Lifecycle, LifecycleContext,
        LinkedNativePluginFactory, NativePluginFactory, NativePluginFactoryContext,
        NativePluginInstance, PluginObject, RuntimeFailure, link_native_plugin,
    };
    pub use futures;
    pub use futures::future::LocalBoxFuture;
    pub use lenso_kernel::{
        ActivateContext, DeactivateContext, InvocationContext, NativeEventEndpoint,
        NativeRequestEndpoint, NativeRequestFuture, NativeStreamEndpoint, NativeStreamSession,
        PluginFuture, PluginLifecycle, PrepareContext,
    };
    pub use lenso_plugin_authoring::{
        BoundCapabilityClient, CapabilityClient, CapabilityClientMany,
    };
    pub use lenso_runtime_codec::InstanceResources;
    pub use serde_json;
}

use lenso_kernel::{
    NativeEndpointSet, NativeEventEndpoint, NativeExecutionAdapter, NativeRequestEndpoint,
    NativeStreamEndpoint, NoopPluginLifecycle, PluginLifecycle, PreparedBinding,
    PreparedEventBinding, PreparedNativeApp, PreparedNativePlugin, PreparedStreamBinding,
};

/// One native Plugin factory contributed to the Host's link-time catalog.
#[derive(Clone, Copy, Debug)]
#[doc(hidden)]
pub struct LinkedNativePluginFactory {
    constructor: fn() -> Rc<dyn NativePluginFactory>,
    descriptor: &'static str,
}

impl LinkedNativePluginFactory {
    /// Creates a link-time catalog record. Intended for generated authoring glue.
    #[doc(hidden)]
    pub const fn new(
        constructor: fn() -> Rc<dyn NativePluginFactory>,
        descriptor: &'static str,
    ) -> Self {
        Self {
            constructor,
            descriptor,
        }
    }
}

inventory::collect!(LinkedNativePluginFactory);

fn explicitly_linked_factories() -> &'static Mutex<Vec<LinkedNativePluginFactory>> {
    static FACTORIES: OnceLock<Mutex<Vec<LinkedNativePluginFactory>>> = OnceLock::new();
    FACTORIES.get_or_init(|| Mutex::new(Vec::new()))
}

/// Retains one generated native Plugin registration through an explicit Host link call.
#[doc(hidden)]
pub fn link_native_plugin(factory: LinkedNativePluginFactory) {
    let mut factories = explicitly_linked_factories()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !factories.iter().any(|linked| {
        linked.descriptor == factory.descriptor
            && std::ptr::fn_addr_eq(linked.constructor, factory.constructor)
    }) {
        factories.push(factory);
    }
}

fn linked_factories() -> Vec<LinkedNativePluginFactory> {
    let factories = inventory::iter::<LinkedNativePluginFactory>
        .into_iter()
        .copied()
        .collect::<Vec<_>>();
    let mut factories = factories;
    factories.extend(
        explicitly_linked_factories()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .copied(),
    );
    factories
        .into_iter()
        .fold(Vec::new(), |mut unique, factory| {
            if !unique.iter().any(|linked: &LinkedNativePluginFactory| {
                linked.descriptor == factory.descriptor
                    && std::ptr::fn_addr_eq(linked.constructor, factory.constructor)
            }) {
                unique.push(factory);
            }
            unique
        })
}

/// Endpoints created for one statically linked Plugin Instance generation.
#[derive(Debug)]
pub struct NativePluginInstance {
    endpoints: NativeEndpointSet,
    lifecycle: Rc<dyn PluginLifecycle>,
}

impl NativePluginInstance {
    /// Creates a generation from its exact declared endpoint set.
    pub fn new(endpoints: Vec<Rc<dyn NativeRequestEndpoint>>) -> Self {
        Self::with_lifecycle(endpoints, NoopPluginLifecycle)
    }

    /// Creates a generation with its exact endpoints and lifecycle Interface.
    pub fn with_lifecycle(
        endpoints: Vec<Rc<dyn NativeRequestEndpoint>>,
        lifecycle: impl PluginLifecycle,
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
        lifecycle: impl PluginLifecycle,
    ) -> Self {
        Self {
            endpoints: NativeEndpointSet::new(endpoints, stream_endpoints, Vec::new()),
            lifecycle: Rc::new(lifecycle),
        }
    }

    /// Creates a generation containing only bidirectional stream endpoints.
    pub fn with_stream_endpoints(
        stream_endpoints: Vec<Rc<dyn NativeStreamEndpoint>>,
        lifecycle: impl PluginLifecycle,
    ) -> Self {
        Self::with_endpoints(Vec::new(), stream_endpoints, lifecycle)
    }

    /// Creates a generation containing only ephemeral Event endpoints.
    pub fn with_event_endpoints(
        event_endpoints: Vec<Rc<dyn NativeEventEndpoint>>,
        lifecycle: impl PluginLifecycle,
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
        lifecycle: impl PluginLifecycle,
    ) -> Self {
        Self {
            endpoints: NativeEndpointSet::new(endpoints, stream_endpoints, event_endpoints),
            lifecycle: Rc::new(lifecycle),
        }
    }

    /// Returns the lifecycle Interface for this generation.
    pub fn lifecycle(&self) -> Rc<dyn PluginLifecycle> {
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

impl Default for NativePluginInstance {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

/// Adapter-specific factory for a statically linked native Rust Plugin.
pub trait NativePluginFactory: std::fmt::Debug + 'static {
    /// Package identity selected by the Resolved App Plan.
    fn package_id(&self) -> &'static str;
    /// Exact statically linked Cargo package version.
    fn package_version(&self) -> &'static str {
        ""
    }
    /// Exact authoring/runtime protocol implemented by this factory.
    fn runtime_profile(&self) -> &'static str {
        "lenso.native-authoring@1"
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
    /// Creates a fresh Plugin Instance generation.
    fn instantiate(
        &self,
        context: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure>;
}

/// Immutable Plan input supplied when a native factory creates one generation.
#[derive(Clone, Copy, Debug)]
pub struct NativePluginFactoryContext<'a> {
    instance_key: &'a str,
    entrypoint: &'a str,
    configuration: &'a str,
    resources: &'a InstanceResources,
}

impl<'a> NativePluginFactoryContext<'a> {
    fn from_plan(
        instance: &'a lenso_app_plan::PluginInstancePlan,
        resources: &'a InstanceResources,
    ) -> Self {
        Self {
            instance_key: instance.instance_key(),
            entrypoint: instance.entrypoint(),
            configuration: instance.configuration(),
            resources,
        }
    }

    /// Returns the App-local Plugin Instance key.
    pub const fn instance_key(self) -> &'a str {
        self.instance_key
    }

    /// Returns the exact package entrypoint selected before boot.
    pub const fn entrypoint(self) -> &'a str {
        self.entrypoint
    }

    /// Returns opaque Plugin-owned configuration selected before boot.
    pub const fn configuration(self) -> &'a str {
        self.configuration
    }

    /// Returns immutable supporting files snapshotted for this Generation.
    pub const fn resources(self) -> &'a InstanceResources {
        self.resources
    }
}

/// Statically linked native Plugin factories available to an App binary.
#[derive(Debug, Default)]
pub struct NativePluginRegistry {
    factories: Vec<Rc<dyn NativePluginFactory>>,
    resources: lenso_runtime_codec::InstanceResourceCatalog,
}

type NativeInstances = BTreeMap<String, NativePluginInstance>;
type PreparedGenerations = BTreeMap<String, PreparedNativePlugin>;
type NativeBindings = (
    Vec<PreparedBinding>,
    Vec<PreparedStreamBinding>,
    Vec<PreparedEventBinding>,
);

fn factory_matches(
    factory: &dyn NativePluginFactory,
    expected: &lenso_app_plan::PluginInstancePlan,
) -> bool {
    factory.package_id() == expected.package_id()
        && factory.runtime_profile() == expected.runtime_profile()
        && (expected.package_revision().is_empty()
            || factory.package_version() == expected.package_revision()
            || factory.factory_identity() == expected.package_revision())
}

impl NativePluginRegistry {
    /// Creates an empty linked-factory registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds every Plugin factory contributed to this Host at link time.
    ///
    /// This catalog describes code available in the binary. The Resolved App
    /// Plan remains the sole authority that selects and binds Plugin Instances.
    #[must_use]
    pub fn with_linked_factories(mut self) -> Self {
        self.factories.extend(
            linked_factories()
                .into_iter()
                .map(|linked| (linked.constructor)()),
        );
        self.factories
            .sort_by_key(|factory| factory.factory_identity());
        self.factories
            .dedup_by(|left, right| left.factory_identity() == right.factory_identity());
        self
    }

    /// Injects exact Generation-bound supporting files for selected Instances.
    #[must_use]
    pub fn with_resources(
        mut self,
        resources: lenso_runtime_codec::InstanceResourceCatalog,
    ) -> Self {
        self.resources = resources;
        self
    }

    /// Returns the exact native factories available to this registry.
    pub fn factories(&self) -> impl Iterator<Item = &dyn NativePluginFactory> {
        self.factories.iter().map(std::convert::AsRef::as_ref)
    }

    /// Builds the immutable Host Catalog declared by this binary and Host policy.
    pub fn host_catalog(
        slots: impl IntoIterator<Item = HostSlot>,
        defaults: impl IntoIterator<Item = HostDefaultPlugin>,
    ) -> Result<HostCatalog, RuntimeFailure> {
        let plugins = linked_factories()
            .into_iter()
            .map(|linked| {
                serde_json::from_str::<PluginDescriptor>(linked.descriptor)
                    .map(HostPluginRelease::new)
                    .map_err(|error| RuntimeFailure::InvalidResolvedPlan {
                        detail: format!("invalid linked Plugin Descriptor: {error}"),
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(HostCatalog::new(slots, plugins, defaults))
    }
    /// Adds one statically linked factory.
    #[must_use]
    pub fn with_factory(mut self, factory: impl NativePluginFactory) -> Self {
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
            .plugin_instances()
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
                    return Err(RuntimeFailure::MissingPluginFactory {
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
            let generation = factory.instantiate(NativePluginFactoryContext::from_plan(
                expected,
                self.resources.for_instance(expected.instance_key()),
            ))?;
            generations.insert(
                expected.instance_key().to_owned(),
                PreparedNativePlugin::with_endpoint_set_lifecycle(
                    generation.endpoints.clone(),
                    generation.lifecycle(),
                ),
            );
            if instances
                .insert(expected.instance_key().to_owned(), generation)
                .is_some()
            {
                return invalid(format!(
                    "duplicate Plugin Instance `{}`",
                    expected.instance_key()
                ));
            }
        }
        Ok((instances, generations))
    }
}

impl NativeExecutionAdapter for NativePluginRegistry {
    fn supports_runtime_profile(&self, authoring_version: u32, profile: &str) -> bool {
        matches!(
            (authoring_version, profile),
            (1, "lenso.native-authoring@1" | "lenso.native-rust@1")
                | (2, "lenso.native-authoring@2")
        )
    }

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
    ) -> Result<PreparedNativePlugin, RuntimeFailure> {
        let expected = plan
            .plugin_instances()
            .iter()
            .find(|instance| instance.instance_key() == instance_key)
            .ok_or_else(|| RuntimeFailure::InvalidResolvedPlan {
                detail: format!("unknown Plugin Instance `{instance_key}`"),
            })?;
        let matching_factories: Vec<_> = self
            .factories
            .iter()
            .filter(|factory| factory_matches(factory.as_ref(), expected))
            .collect();
        let factory = match matching_factories.as_slice() {
            [] => {
                return Err(RuntimeFailure::MissingPluginFactory {
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
        let generation = factory.instantiate(NativePluginFactoryContext::from_plan(
            expected,
            self.resources.for_instance(expected.instance_key()),
        ))?;
        Ok(PreparedNativePlugin::with_endpoint_set_lifecycle(
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
            .plugin_instance(binding.provider_instance())
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
            bindings.push(
                PreparedBinding::new(
                    binding.consumer_instance(),
                    binding.provider_instance(),
                    endpoint,
                )
                .with_requirement_id(binding.requirement_id()),
            );
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
            stream_bindings.push(
                PreparedStreamBinding::new(
                    binding.consumer_instance(),
                    binding.provider_instance(),
                    endpoint,
                )
                .with_requirement_id(binding.requirement_id()),
            );
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
            event_bindings.push(
                PreparedEventBinding::new(
                    binding.consumer_instance(),
                    binding.provider_instance(),
                    endpoint,
                )
                .with_requirement_id(binding.requirement_id()),
            );
        }
    }
    Ok((bindings, stream_bindings, event_bindings))
}

fn invalid<T>(detail: String) -> Result<T, RuntimeFailure> {
    Err(RuntimeFailure::InvalidResolvedPlan { detail })
}
