use std::{collections::BTreeMap, rc::Rc, sync::Arc, time::Instant};

use lenso_app_plan::{
    AppComposition, CapabilityBinding, ExecutionClassId, ExecutionLaneId, ExecutionLanePlan,
    ModuleInstancePlan, ResolvedAppPlan,
};
use lenso_kernel::{
    ExecutionAdapter, NoopModuleLifecycle, PreparedBinding, PreparedNativeApp,
    PreparedNativeModule, RuntimeFailure,
};

use super::{CrossLaneRequestCatalog, LANE_PROXY_EXECUTION_CLASS, LaneRoute};

pub(super) fn project_lane(
    plan: &ResolvedAppPlan,
    lane: &ExecutionLaneId,
) -> Result<ResolvedAppPlan, super::ReplicatedRunnerError> {
    let bindings = plan
        .capability_bindings()
        .iter()
        .filter(|binding| binding_touches_lane(plan, binding, lane))
        .cloned()
        .collect::<Vec<_>>();
    let mut instances = plan
        .module_instances()
        .iter()
        .filter(|instance| instance.execution_lane() == lane)
        .map(|instance| clone_instance(instance, lane))
        .collect::<BTreeMap<_, _>>();

    for binding in &bindings {
        let consumer = plan
            .module_instance(binding.consumer_instance())
            .expect("validated binding consumer should exist");
        let provider = plan
            .module_instance(binding.provider_instance())
            .expect("validated binding provider should exist");
        if consumer.execution_lane() == lane && provider.execution_lane() != lane {
            add_provider_proxy(&mut instances, provider, binding, lane);
        }
        if provider.execution_lane() == lane && consumer.execution_lane() != lane {
            add_consumer_proxy(&mut instances, consumer, binding, lane);
        }
    }

    AppComposition::new(instances.into_values().collect(), bindings)
        .with_execution_lanes(vec![ExecutionLanePlan::new(lane.as_str())])
        .resolve()
        .map_err(|error| super::ReplicatedRunnerError::InvalidPlan {
            detail: format!("lane `{lane}` projection failed: {error}"),
        })
}

fn binding_touches_lane(
    plan: &ResolvedAppPlan,
    binding: &CapabilityBinding,
    lane: &ExecutionLaneId,
) -> bool {
    [binding.consumer_instance(), binding.provider_instance()]
        .into_iter()
        .filter_map(|instance| plan.module_instance(instance))
        .any(|instance| instance.execution_lane() == lane)
}

fn clone_instance(
    source: &ModuleInstancePlan,
    lane: &ExecutionLaneId,
) -> (String, ModuleInstancePlan) {
    let mut instance = clone_instance_identity(source, lane, source.execution_class().clone());
    for capability in source.provided_capabilities() {
        instance = instance.with_capability(capability.clone());
    }
    for requirement in source.required_capabilities() {
        instance = instance.with_requirement(requirement.clone());
    }
    (source.instance_key().to_owned(), instance)
}

fn clone_instance_identity(
    source: &ModuleInstancePlan,
    lane: &ExecutionLaneId,
    execution_class: ExecutionClassId,
) -> ModuleInstancePlan {
    ModuleInstancePlan::new(source.instance_key(), source.package_id())
        .with_entrypoint(source.entrypoint())
        .with_configuration(source.configuration())
        .with_execution_class(execution_class)
        .with_execution_lane(lane.clone())
        .with_package_revision(source.package_revision())
        .with_restart_policy(source.restart_policy())
        .with_criticality(source.criticality())
}

fn add_provider_proxy(
    instances: &mut BTreeMap<String, ModuleInstancePlan>,
    source: &ModuleInstancePlan,
    binding: &CapabilityBinding,
    lane: &ExecutionLaneId,
) {
    let instance = instances
        .entry(source.instance_key().to_owned())
        .or_insert_with(|| {
            clone_instance_identity(
                source,
                lane,
                ExecutionClassId::new(LANE_PROXY_EXECUTION_CLASS),
            )
        });
    if instance
        .provided_capabilities()
        .iter()
        .all(|endpoint| endpoint.capability_id() != binding.capability_id())
    {
        let endpoint = source
            .provided_capabilities()
            .iter()
            .find(|endpoint| endpoint.capability_id() == binding.capability_id())
            .expect("validated provider endpoint should exist")
            .clone();
        *instance = instance.clone().with_capability(endpoint);
    }
}

fn add_consumer_proxy(
    instances: &mut BTreeMap<String, ModuleInstancePlan>,
    source: &ModuleInstancePlan,
    binding: &CapabilityBinding,
    lane: &ExecutionLaneId,
) {
    let instance = instances
        .entry(source.instance_key().to_owned())
        .or_insert_with(|| {
            clone_instance_identity(
                source,
                lane,
                ExecutionClassId::new(LANE_PROXY_EXECUTION_CLASS),
            )
        });
    if instance
        .required_capabilities()
        .iter()
        .all(|requirement| requirement.capability_id() != binding.capability_id())
    {
        let requirement = source
            .required_capabilities()
            .iter()
            .find(|requirement| requirement.capability_id() == binding.capability_id())
            .expect("validated consumer requirement should exist")
            .clone();
        *instance = instance.clone().with_requirement(requirement);
    }
}

#[derive(Clone, Debug)]
pub(super) struct LaneProxyAdapter {
    full_plan: Arc<ResolvedAppPlan>,
    transfers: CrossLaneRequestCatalog,
    routes: Arc<BTreeMap<ExecutionLaneId, LaneRoute>>,
    epoch: Instant,
}

impl LaneProxyAdapter {
    pub(super) fn new(
        full_plan: Arc<ResolvedAppPlan>,
        transfers: CrossLaneRequestCatalog,
        routes: Arc<BTreeMap<ExecutionLaneId, LaneRoute>>,
        epoch: Instant,
    ) -> Self {
        Self {
            full_plan,
            transfers,
            routes,
            epoch,
        }
    }
}

impl ExecutionAdapter for LaneProxyAdapter {
    fn execution_class(&self) -> ExecutionClassId {
        ExecutionClassId::new(LANE_PROXY_EXECUTION_CLASS)
    }

    fn prepare(&self, plan: &ResolvedAppPlan) -> Result<PreparedNativeApp, RuntimeFailure> {
        let mut generations = BTreeMap::new();
        let mut endpoints = BTreeMap::new();
        for instance in plan
            .module_instances()
            .iter()
            .filter(|instance| instance.execution_class().as_str() == LANE_PROXY_EXECUTION_CLASS)
        {
            let mut generation_endpoints = Vec::new();
            for descriptor in instance.provided_capabilities() {
                let source = self
                    .full_plan
                    .module_instance(instance.instance_key())
                    .expect("proxy provider exists in the full Plan");
                let sender = self
                    .routes
                    .get(source.execution_lane())
                    .ok_or_else(|| RuntimeFailure::InvalidResolvedPlan {
                        detail: format!(
                            "provider lane `{}` is absent for `{}`",
                            source.execution_lane(),
                            instance.instance_key()
                        ),
                    })?
                    .clone();
                let endpoint = self
                    .transfers
                    .endpoint(descriptor.capability_id(), sender, self.epoch)
                    .ok_or_else(|| RuntimeFailure::InvalidResolvedPlan {
                        detail: format!(
                            "Capability `{}` has no registered native cross-lane request transfer",
                            descriptor.capability_id()
                        ),
                    })?;
                endpoints.insert(
                    (
                        instance.instance_key().to_owned(),
                        descriptor.capability_id().to_owned(),
                    ),
                    Rc::clone(&endpoint),
                );
                generation_endpoints.push(endpoint);
            }
            generations.insert(
                instance.instance_key().to_owned(),
                PreparedNativeModule::new(generation_endpoints, NoopModuleLifecycle),
            );
        }
        let bindings = plan
            .capability_bindings()
            .iter()
            .filter_map(|binding| {
                endpoints
                    .get(&(
                        binding.provider_instance().to_owned(),
                        binding.capability_id().to_owned(),
                    ))
                    .map(|endpoint| {
                        PreparedBinding::new(
                            binding.consumer_instance(),
                            binding.provider_instance(),
                            Rc::clone(endpoint),
                        )
                    })
            })
            .collect();
        Ok(PreparedNativeApp::new(bindings, generations))
    }
}
