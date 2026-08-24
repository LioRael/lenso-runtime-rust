use std::collections::{BTreeMap, BTreeSet};

use lenso_app_plan::{
    AppComposition, CapabilityBinding, CapabilityCardinality, CapabilityEndpointPlan,
    CapabilityRequirementPlan, ExecutionClassId, ExecutionLaneId, ModuleInstancePlan,
};
use lenso_runtime_codec::{ArtifactCatalog, ArtifactHandle};

use crate::{
    AdapterProfile, AppGenerationSpec, ArtifactKind, CanonicalDocument, ClassPolicy,
    ControlPlaneError, EffectiveGrant, EffectiveHostGrantSet, HostBuildManifest,
    HostExecutionPolicy, ImplementationVariant, LockedInstance, PluginManifest, PluginSetLock,
    RequirementCardinality, ResolvedArtifact, ResolvedArtifactSet, ResolvedDataMount,
    ResolvedInstance, ResolvedRelease, StatefulRuntimeIdentity,
};
use crate::{PluginStore, sha256_digest, strict_json};

#[derive(Debug)]
struct SelectedReleaseContent {
    modules: BTreeSet<String>,
    data: BTreeSet<String>,
    explicit_artifacts: BTreeSet<String>,
    artifacts: BTreeSet<String>,
    permissions: BTreeSet<String>,
    product_metadata_digests: BTreeSet<String>,
}

/// Complete immutable authority supplied to deterministic Plugin resolution.
#[derive(Debug)]
pub struct ResolutionInput<'a> {
    pub lock: &'a CanonicalDocument<PluginSetLock>,
    pub manifests: &'a BTreeMap<String, CanonicalDocument<PluginManifest>>,
    pub admission_receipts: &'a BTreeMap<String, String>,
    pub host_build: &'a CanonicalDocument<HostBuildManifest>,
    pub policy: &'a CanonicalDocument<HostExecutionPolicy>,
    pub store: &'a PluginStore,
    pub base_instances: Vec<ModuleInstancePlan>,
    pub bindings: Vec<CapabilityBinding>,
}

/// Exact output used to stage one immutable App Generation.
#[derive(Clone, Debug)]
pub struct ResolvedGeneration {
    pub plan: lenso_app_plan::ResolvedAppPlan,
    pub artifact_set: CanonicalDocument<ResolvedArtifactSet>,
    pub grants: CanonicalDocument<EffectiveHostGrantSet>,
    pub spec: CanonicalDocument<AppGenerationSpec>,
    pub artifacts: ArtifactCatalog,
    /// Exact stateful Instance identities used by Transition compatibility checks.
    pub stateful_instances: BTreeMap<String, StatefulRuntimeIdentity>,
}

/// Selects one exact implementation per Instance and closes all Generation authority.
#[allow(clippy::too_many_lines)]
pub fn resolve_generation(
    input: &ResolutionInput<'_>,
) -> Result<ResolvedGeneration, ControlPlaneError> {
    validate_top_level(input)?;
    let mut plugin_instances = Vec::new();
    let mut resolved_instances = Vec::new();
    let mut resolved_artifacts = BTreeMap::<(String, String), ResolvedArtifact>::new();
    let mut artifact_catalog = ArtifactCatalog::new();
    let mut stateful_instances = BTreeMap::new();

    for locked in &input.lock.value().instances {
        let manifest = manifest_for(input, &locked.plugin_id)?;
        let selected_content = selected_release_content(input, &locked.plugin_id)?;
        let contribution = manifest
            .module_contributions
            .iter()
            .find(|candidate| candidate.id == locked.contribution_id)
            .ok_or_else(|| {
                failed(format!(
                    "unknown contribution `{}` for Plugin `{}`",
                    locked.contribution_id, locked.plugin_id
                ))
            })?;
        if !selected_content.modules.contains(&contribution.id) {
            return Err(failed(format!(
                "contribution `{}` is not selected by Plugin Features",
                contribution.id
            )));
        }
        ensure_required_grants(
            locked,
            contribution.permission_request_ids.as_slice(),
            input,
        )?;
        let variant = select_variant(locked, contribution.implementations.as_slice(), input)?;
        if variant
            .artifact
            .as_ref()
            .is_some_and(|artifact| !selected_content.artifacts.contains(artifact))
        {
            return Err(failed(format!(
                "implementation Artifact for `{}` is not selected by Plugin Features",
                locked.instance_key
            )));
        }
        if let Some(factory) = &variant.built_in_factory
            && !input
                .host_build
                .value()
                .built_in_modules
                .iter()
                .any(|built_in| {
                    built_in.package_id == contribution.package_id
                        && built_in.factory_identity == *factory
                        && built_in.execution_class == variant.execution_class
                })
        {
            return Err(failed(format!(
                "built-in factory `{factory}` is absent from the exact Host Build Manifest"
            )));
        }
        let revision = if let Some(artifact_id) = &variant.artifact {
            let declaration = manifest
                .artifacts
                .iter()
                .find(|artifact| &artifact.id == artifact_id)
                .ok_or_else(|| failed(format!("unknown Artifact `{artifact_id}`")))?;
            if !artifact_kind_matches_execution(declaration.kind, &variant.execution_class) {
                return Err(failed(format!(
                    "Artifact `{artifact_id}` kind does not match execution class `{}`",
                    variant.execution_class
                )));
            }
            let admitted = input.store.artifact(declaration)?;
            let handle = ArtifactHandle::open(&admitted.path, &admitted.digest, admitted.size)
                .map_err(|error| failed(format!("{error:?}")))?;
            artifact_catalog = artifact_catalog
                .with_artifact(&locked.instance_key, handle)
                .map_err(|error| failed(format!("{error:?}")))?;
            resolved_artifacts
                .entry((locked.plugin_id.clone(), artifact_id.clone()))
                .or_insert_with(|| ResolvedArtifact {
                    plugin_id: locked.plugin_id.clone(),
                    artifact_id: artifact_id.clone(),
                    digest: declaration.digest.clone(),
                    size: declaration.size,
                    media_type: declaration.media_type.clone(),
                    target: input.policy.value().target.clone(),
                });
            declaration.digest.clone()
        } else {
            variant
                .built_in_factory
                .clone()
                .expect("variant execution input was validated")
        };
        if let Some(state) = &contribution.state {
            let locked_plugin = input
                .lock
                .value()
                .plugins
                .iter()
                .find(|plugin| plugin.plugin_id == locked.plugin_id)
                .expect("manifest lookup validated the locked Plugin");
            let runtime_identity = if variant.artifact.is_some() {
                format!("plugin:{}:{}", locked_plugin.manifest_digest, revision)
            } else {
                format!("builtin:{}:{}", input.host_build.digest(), revision)
            };
            if stateful_instances
                .insert(
                    locked.instance_key.clone(),
                    StatefulRuntimeIdentity {
                        runtime_identity,
                        state_schema_id: state.state_schema_id.clone(),
                        state_schema_digest: state.state_schema_digest.clone(),
                    },
                )
                .is_some()
            {
                return Err(failed(format!(
                    "duplicate state authority for Instance `{}`",
                    locked.instance_key
                )));
            }
        }

        let configuration: serde_json::Value =
            strict_json("Instance configuration", locked.configuration.as_bytes())
                .map_err(|error| failed(error.to_string()))?;
        if serde_json::to_string(&configuration).map_err(|error| failed(error.to_string()))?
            != locked.configuration
        {
            return Err(failed(format!(
                "Instance `{}` configuration is not canonical JSON",
                locked.instance_key
            )));
        }
        let mut instance = ModuleInstancePlan::new(&locked.instance_key, &contribution.package_id)
            .with_entrypoint(&variant.entrypoint)
            .with_configuration(&locked.configuration)
            .with_execution_class(ExecutionClassId::new(&variant.execution_class))
            .with_execution_lane(ExecutionLaneId::new(&locked.execution_lane))
            .with_package_revision(revision);
        for provided in &contribution.provides {
            instance = instance.with_capability(CapabilityEndpointPlan::new(
                &provided.capability_id,
                &provided.descriptor_version,
                provided.request_operations.clone(),
            ));
        }
        for required in &contribution.requires {
            let cardinality = match required.cardinality {
                RequirementCardinality::One => CapabilityCardinality::One,
                RequirementCardinality::Optional => CapabilityCardinality::Optional,
                RequirementCardinality::Many => CapabilityCardinality::Many,
            };
            instance = instance.with_requirement(CapabilityRequirementPlan::new(
                &required.capability_id,
                &required.descriptor_version,
                cardinality,
            ));
        }
        plugin_instances.push(instance);
        resolved_instances.push(ResolvedInstance {
            instance_key: locked.instance_key.clone(),
            plugin_id: locked.plugin_id.clone(),
            contribution_id: locked.contribution_id.clone(),
            implementation_variant: variant.id.clone(),
            artifact_id: variant.artifact.clone(),
            built_in_factory: variant.built_in_factory.clone(),
            entrypoint: variant.entrypoint.clone(),
            execution_class: variant.execution_class.clone(),
            target: input.policy.value().target.clone(),
            support_channel: variant.support_channel,
            selection_reason: locked.implementation_variant.as_ref().map_or_else(
                || "host_execution_policy_preference".to_owned(),
                |_| "app_lock_exact_pin".to_owned(),
            ),
            profiles: variant.profiles.clone(),
            limits: BTreeMap::new(),
            provided_capabilities: contribution.provides.clone(),
            required_capabilities: contribution.requires.clone(),
        });
    }

    let mut instances = input.base_instances.clone();
    instances.extend(plugin_instances);
    let plan = AppComposition::new(instances, input.bindings.clone())
        .resolve()
        .map_err(|error| failed(format!("resolved Plan is invalid: {error}")))?;
    verify_plugin_plan_closure(&plan, &resolved_instances)?;
    verify_binding_template_closure(input)?;

    let releases = input
        .lock
        .value()
        .plugins
        .iter()
        .map(|locked| {
            let receipt_digest = input
                .admission_receipts
                .get(&locked.manifest_digest)
                .ok_or_else(|| {
                    failed(format!(
                        "Release `{}` has no Admission Receipt",
                        locked.plugin_id
                    ))
                })?;
            let receipt = input.store.admission_receipt(receipt_digest)?;
            let manifest = manifest_for(input, &locked.plugin_id)?;
            let artifact_digests = manifest
                .artifacts
                .iter()
                .map(|artifact| artifact.digest.clone())
                .collect::<BTreeSet<_>>();
            let metadata_digests = manifest
                .product_metadata
                .iter()
                .map(|metadata| metadata.digest.clone())
                .collect::<BTreeSet<_>>();
            if receipt.value().schema_version != 1
                || receipt.value().plugin_id != locked.plugin_id
                || receipt.value().release_version != locked.release_version
                || receipt.value().manifest_digest != locked.manifest_digest
                || receipt
                    .value()
                    .artifact_digests
                    .iter()
                    .cloned()
                    .collect::<BTreeSet<_>>()
                    != artifact_digests
                || receipt
                    .value()
                    .product_metadata_digests
                    .iter()
                    .cloned()
                    .collect::<BTreeSet<_>>()
                    != metadata_digests
            {
                return Err(failed(format!(
                    "Release `{}` Admission Receipt does not close over its Manifest",
                    locked.plugin_id
                )));
            }
            Ok(ResolvedRelease {
                plugin_id: locked.plugin_id.clone(),
                release_version: locked.release_version.clone(),
                manifest_digest: locked.manifest_digest.clone(),
                admission_receipt_digest: receipt_digest.clone(),
            })
        })
        .collect::<Result<Vec<_>, ControlPlaneError>>()?;
    let data_mounts = resolve_data_mounts(input, &mut resolved_artifacts)?;
    resolve_selected_artifacts(input, &mut resolved_artifacts)?;
    let artifact_set = CanonicalDocument::from_value(
        "lenso-artifacts.lock.json",
        ResolvedArtifactSet {
            schema_version: 1,
            plugin_set_lock_digest: input.lock.digest().to_owned(),
            host_execution_policy_digest: input.policy.digest().to_owned(),
            releases,
            artifacts: resolved_artifacts.into_values().collect(),
            instances: resolved_instances,
            data_mounts,
        },
    )?;
    let grants = CanonicalDocument::from_value(
        "lenso-host-grants.lock.json",
        EffectiveHostGrantSet {
            schema_version: 1,
            plugin_set_lock_digest: input.lock.digest().to_owned(),
            grants: input
                .lock
                .value()
                .approved_grants
                .iter()
                .map(|grant| EffectiveGrant {
                    instance_key: grant.instance_key.clone(),
                    permission_request_id: grant.permission_request_id.clone(),
                    scope: grant.scope.clone(),
                    enforcement_kind: grant.enforcement_kind,
                    enforcer_identity: grant.enforcer_identity.clone(),
                    configuration: serde_json::Value::Object(serde_json::Map::new()),
                })
                .collect(),
        },
    )?;
    let plan_bytes = serde_json::to_vec(&plan).map_err(|error| failed(error.to_string()))?;
    let spec = CanonicalDocument::from_value(
        "lenso-generation.json",
        AppGenerationSpec {
            schema_version: 1,
            app_id: input.lock.value().app_id.clone(),
            host_build_manifest_digest: input.host_build.digest().to_owned(),
            host_execution_policy_digest: input.policy.digest().to_owned(),
            resolved_plan_digest: sha256_digest(&plan_bytes),
            plugin_set_lock_digest: input.lock.digest().to_owned(),
            resolved_artifact_set_digest: artifact_set.digest().to_owned(),
            effective_host_grant_set_digest: grants.digest().to_owned(),
        },
    )?;
    Ok(ResolvedGeneration {
        plan,
        artifact_set,
        grants,
        spec,
        artifacts: artifact_catalog,
        stateful_instances,
    })
}

#[allow(clippy::too_many_lines)]
fn validate_top_level(input: &ResolutionInput<'_>) -> Result<(), ControlPlaneError> {
    let lock = input.lock.value();
    let build = input.host_build.value();
    let policy = input.policy.value();
    if lock.schema_version != 1 || build.schema_version != 1 || policy.schema_version != 1 {
        return Err(failed("unsupported control-plane schema version"));
    }
    if lock.app_id != build.app_id || lock.app_id != policy.app_id {
        return Err(failed(
            "App identity differs across control-plane authority",
        ));
    }
    if policy.host_build_manifest_digest != input.host_build.digest() {
        return Err(failed(
            "Host Execution Policy does not bind the Host Build Manifest",
        ));
    }
    if policy.target != build.target {
        return Err(failed(
            "Host target differs between policy and build manifest",
        ));
    }
    ensure_sorted_unique(
        lock.plugins.iter().map(|plugin| &plugin.plugin_id),
        "Plugin ID",
    )?;
    ensure_sorted_unique(
        lock.instances.iter().map(|instance| &instance.instance_key),
        "Instance key",
    )?;
    ensure_sorted_unique(
        policy.classes.iter().map(|class| &class.execution_class),
        "class policy",
    )?;
    ensure_unique(policy.preference.iter(), "class preference")?;
    ensure_sorted_unique(
        policy
            .instance_overrides
            .iter()
            .map(|rule| &rule.instance_key),
        "Instance override",
    )?;
    ensure_sorted_unique(
        build
            .built_in_modules
            .iter()
            .map(|module| &module.factory_identity),
        "built-in factory",
    )?;
    ensure_sorted_unique(
        build
            .adapter_profiles
            .iter()
            .map(|profile| &profile.execution_class),
        "Adapter profile",
    )?;
    let mounts = lock
        .data_mounts
        .iter()
        .map(|mount| {
            format!(
                "{}\0{}\0{}\0{}",
                mount.plugin_id,
                mount.contribution_id,
                mount.interpreter_instance_key,
                mount.input_slot
            )
        })
        .collect::<Vec<_>>();
    ensure_owned_sorted_unique(&mounts, "Data mount")?;
    let grants = lock
        .approved_grants
        .iter()
        .map(|grant| format!("{}\0{}", grant.instance_key, grant.permission_request_id))
        .collect::<Vec<_>>();
    ensure_owned_sorted_unique(&grants, "approved grant")?;
    for plugin in &lock.plugins {
        let selected = selected_release_content(input, &plugin.plugin_id)?;
        let selected_instances = lock
            .instances
            .iter()
            .filter(|instance| instance.plugin_id == plugin.plugin_id)
            .map(|instance| instance.contribution_id.clone())
            .collect::<BTreeSet<_>>();
        if selected_instances != selected.modules {
            return Err(failed(format!(
                "Plugin `{}` Module Feature expansion does not exactly match locked Instances",
                plugin.plugin_id
            )));
        }
        let selected_data = lock
            .data_mounts
            .iter()
            .filter(|mount| mount.plugin_id == plugin.plugin_id)
            .map(|mount| mount.contribution_id.clone())
            .collect::<BTreeSet<_>>();
        if selected_data != selected.data {
            return Err(failed(format!(
                "Plugin `{}` Data Feature expansion does not exactly match locked mounts",
                plugin.plugin_id
            )));
        }
    }
    for grant in &lock.approved_grants {
        let instance = lock
            .instances
            .iter()
            .find(|instance| instance.instance_key == grant.instance_key)
            .ok_or_else(|| failed("approved grant references an unknown Plugin Instance"))?;
        let manifest = manifest_for(input, &instance.plugin_id)?;
        let selected = selected_release_content(input, &instance.plugin_id)?;
        let request = manifest
            .permission_requests
            .iter()
            .find(|request| request.id == grant.permission_request_id)
            .ok_or_else(|| failed("approved grant references an unknown permission request"))?;
        if !selected.permissions.contains(&request.id)
            || !scope_is_narrower_or_equal(&grant.scope, &request.scope)
            || grant.enforcer_identity.is_empty()
        {
            return Err(failed(
                "grant must select a requested permission, equal-or-narrower scope, and named enforcer",
            ));
        }
    }
    Ok(())
}

fn scope_is_narrower_or_equal(approved: &serde_json::Value, requested: &serde_json::Value) -> bool {
    match (approved, requested) {
        (serde_json::Value::Object(approved), serde_json::Value::Object(requested)) => {
            approved.iter().all(|(key, value)| {
                requested
                    .get(key)
                    .is_some_and(|limit| scope_is_narrower_or_equal(value, limit))
            })
        }
        (serde_json::Value::Array(approved), serde_json::Value::Array(requested)) => approved
            .iter()
            .all(|value| requested.iter().any(|limit| value == limit)),
        _ => approved == requested,
    }
}

fn manifest_for<'a>(
    input: &'a ResolutionInput<'_>,
    plugin_id: &str,
) -> Result<&'a PluginManifest, ControlPlaneError> {
    let locked = input
        .lock
        .value()
        .plugins
        .iter()
        .find(|plugin| plugin.plugin_id == plugin_id)
        .ok_or_else(|| failed(format!("Instance references unlocked Plugin `{plugin_id}`")))?;
    let document = input
        .manifests
        .get(plugin_id)
        .ok_or_else(|| failed(format!("Plugin `{plugin_id}` has no admitted Manifest")))?;
    if document.digest() != locked.manifest_digest
        || document.value().plugin_id != locked.plugin_id
        || document.value().release_version != locked.release_version
    {
        return Err(failed(format!(
            "Plugin `{plugin_id}` Manifest does not match its lock"
        )));
    }
    Ok(document.value())
}

#[allow(clippy::too_many_lines)]
fn selected_release_content(
    input: &ResolutionInput<'_>,
    plugin_id: &str,
) -> Result<SelectedReleaseContent, ControlPlaneError> {
    let locked = input
        .lock
        .value()
        .plugins
        .iter()
        .find(|plugin| plugin.plugin_id == plugin_id)
        .ok_or_else(|| failed(format!("Plugin `{plugin_id}` is not locked")))?;
    let manifest = manifest_for(input, plugin_id)?;
    let selected_feature_ids = locked
        .selected_features
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if selected_feature_ids.len() != locked.selected_features.len()
        || locked
            .selected_features
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        || locked
            .product_metadata_digests
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
    {
        return Err(failed(format!(
            "Plugin `{plugin_id}` Feature or Product Metadata selection is not sorted and unique"
        )));
    }
    let all_feature_modules = manifest
        .features
        .iter()
        .flat_map(|feature| feature.module_contribution_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    let all_feature_data = manifest
        .features
        .iter()
        .flat_map(|feature| feature.data_contribution_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    let all_feature_artifacts = manifest
        .features
        .iter()
        .flat_map(|feature| feature.artifact_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    let all_feature_permissions = manifest
        .features
        .iter()
        .flat_map(|feature| feature.permission_request_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    let all_feature_metadata = manifest
        .features
        .iter()
        .flat_map(|feature| feature.product_metadata_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    let mut selected = SelectedReleaseContent {
        modules: manifest
            .module_contributions
            .iter()
            .filter(|contribution| !all_feature_modules.contains(&contribution.id))
            .map(|contribution| contribution.id.clone())
            .collect(),
        data: manifest
            .data_contributions
            .iter()
            .filter(|contribution| !all_feature_data.contains(&contribution.id))
            .map(|contribution| contribution.id.clone())
            .collect(),
        explicit_artifacts: BTreeSet::new(),
        artifacts: manifest
            .artifacts
            .iter()
            .filter(|artifact| !all_feature_artifacts.contains(&artifact.id))
            .map(|artifact| artifact.id.clone())
            .collect(),
        permissions: manifest
            .permission_requests
            .iter()
            .filter(|request| !all_feature_permissions.contains(&request.id))
            .map(|request| request.id.clone())
            .collect(),
        product_metadata_digests: manifest
            .product_metadata
            .iter()
            .filter(|metadata| !all_feature_metadata.contains(&metadata.id))
            .map(|metadata| metadata.digest.clone())
            .collect(),
    };
    for feature_id in selected_feature_ids {
        let feature = manifest
            .features
            .iter()
            .find(|feature| feature.id == feature_id)
            .ok_or_else(|| failed(format!("unknown selected Feature `{feature_id}`")))?;
        selected
            .modules
            .extend(feature.module_contribution_ids.iter().cloned());
        selected
            .data
            .extend(feature.data_contribution_ids.iter().cloned());
        selected
            .artifacts
            .extend(feature.artifact_ids.iter().cloned());
        selected
            .explicit_artifacts
            .extend(feature.artifact_ids.iter().cloned());
        selected
            .permissions
            .extend(feature.permission_request_ids.iter().cloned());
        for metadata_id in &feature.product_metadata_ids {
            let metadata = manifest
                .product_metadata
                .iter()
                .find(|metadata| &metadata.id == metadata_id)
                .expect("Manifest Feature references were validated at admission");
            selected
                .product_metadata_digests
                .insert(metadata.digest.clone());
        }
    }
    let locked_metadata = locked
        .product_metadata_digests
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if locked_metadata.len() != locked.product_metadata_digests.len()
        || locked_metadata != selected.product_metadata_digests
    {
        return Err(failed(format!(
            "Plugin `{plugin_id}` Product Metadata selection is not exact"
        )));
    }
    Ok(selected)
}

fn select_variant<'a>(
    locked: &LockedInstance,
    variants: &'a [ImplementationVariant],
    input: &ResolutionInput<'_>,
) -> Result<&'a ImplementationVariant, ControlPlaneError> {
    let valid: Vec<_> = variants
        .iter()
        .filter(|variant| variant_allowed(locked, variant, input))
        .collect();
    if let Some(pin) = &locked.implementation_variant {
        return valid
            .into_iter()
            .find(|variant| &variant.id == pin)
            .ok_or_else(|| failed(format!("pinned implementation `{pin}` is not admitted")));
    }
    let preference = input
        .policy
        .value()
        .instance_overrides
        .iter()
        .find(|rule| rule.instance_key == locked.instance_key)
        .map_or(input.policy.value().preference.as_slice(), |rule| {
            rule.preference.as_slice()
        });
    for execution_class in preference {
        let at_rank: Vec<_> = valid
            .iter()
            .copied()
            .filter(|variant| &variant.execution_class == execution_class)
            .collect();
        match at_rank.as_slice() {
            [] => {}
            [selected] => return Ok(*selected),
            _ => {
                return Err(failed(format!(
                    "ambiguous implementations for Instance `{}` at class `{execution_class}`",
                    locked.instance_key
                )));
            }
        }
    }
    Err(failed(format!(
        "no admitted implementation for Instance `{}`",
        locked.instance_key
    )))
}

fn variant_allowed(
    locked: &LockedInstance,
    variant: &ImplementationVariant,
    input: &ResolutionInput<'_>,
) -> bool {
    if !variant.targets.contains(&input.policy.value().target) {
        return false;
    }
    let Some(class_policy) = input
        .policy
        .value()
        .classes
        .iter()
        .find(|class| class.execution_class == variant.execution_class)
    else {
        return false;
    };
    let Some(adapter) = adapter_profile(input.host_build.value(), &variant.execution_class) else {
        return false;
    };
    let override_allows = input
        .policy
        .value()
        .instance_overrides
        .iter()
        .find(|rule| rule.instance_key == locked.instance_key)
        .is_none_or(|rule| rule.allowed_classes.contains(&variant.execution_class));
    override_allows
        && class_allows(class_policy, variant)
        && adapter.targets.contains(&input.policy.value().target)
        && variant
            .profiles
            .iter()
            .all(|profile| adapter.profiles.contains(profile))
}

fn class_allows(policy: &ClassPolicy, variant: &ImplementationVariant) -> bool {
    policy.support_channels.contains(&variant.support_channel)
        && policy.trust_levels.contains(&variant.trust)
        && variant
            .profiles
            .iter()
            .all(|profile| policy.profiles.contains(profile))
}

fn adapter_profile<'a>(build: &'a HostBuildManifest, class: &str) -> Option<&'a AdapterProfile> {
    build
        .adapter_profiles
        .iter()
        .find(|profile| profile.execution_class == class)
}

fn artifact_kind_matches_execution(kind: ArtifactKind, execution_class: &str) -> bool {
    match execution_class {
        "lenso.wasm-component@1" => kind == ArtifactKind::WasmComponent,
        "lenso.quickjs@1" => kind == ArtifactKind::QuickJsModule,
        "lenso.native-dylib@1" => kind == ArtifactKind::NativeDylib,
        _ => kind == ArtifactKind::Process,
    }
}

fn ensure_required_grants(
    locked: &LockedInstance,
    permission_ids: &[String],
    input: &ResolutionInput<'_>,
) -> Result<(), ControlPlaneError> {
    for permission_id in permission_ids {
        if !input.lock.value().approved_grants.iter().any(|grant| {
            grant.instance_key == locked.instance_key
                && &grant.permission_request_id == permission_id
                && !grant.enforcer_identity.is_empty()
        }) {
            return Err(failed(format!(
                "Instance `{}` lacks required grant `{permission_id}`",
                locked.instance_key
            )));
        }
    }
    Ok(())
}

fn resolve_data_mounts(
    input: &ResolutionInput<'_>,
    artifacts: &mut BTreeMap<(String, String), ResolvedArtifact>,
) -> Result<Vec<ResolvedDataMount>, ControlPlaneError> {
    let plan_instance_keys: BTreeSet<_> = input
        .base_instances
        .iter()
        .map(lenso_app_plan::ModuleInstancePlan::instance_key)
        .chain(
            input
                .lock
                .value()
                .instances
                .iter()
                .map(|instance| instance.instance_key.as_str()),
        )
        .collect();
    input
        .lock
        .value()
        .data_mounts
        .iter()
        .map(|mount| {
            if !plan_instance_keys.contains(mount.interpreter_instance_key.as_str()) {
                return Err(failed(format!(
                    "Data mount interpreter `{}` is not a Plan Instance",
                    mount.interpreter_instance_key
                )));
            }
            let manifest = manifest_for(input, &mount.plugin_id)?;
            let selected = selected_release_content(input, &mount.plugin_id)?;
            if !selected.data.contains(&mount.contribution_id) {
                return Err(failed(
                    "Data mount contribution is not selected by Features",
                ));
            }
            let contribution = manifest
                .data_contributions
                .iter()
                .find(|candidate| candidate.id == mount.contribution_id)
                .ok_or_else(|| failed("Data mount references an unknown contribution"))?;
            let declaration = manifest
                .artifacts
                .iter()
                .find(|artifact| artifact.id == contribution.artifact)
                .ok_or_else(|| failed("Data contribution references an unknown Artifact"))?;
            if !selected.artifacts.contains(&declaration.id) {
                return Err(failed("Data Artifact is not selected by Features"));
            }
            let _ = input.store.artifact(declaration)?;
            artifacts
                .entry((mount.plugin_id.clone(), declaration.id.clone()))
                .or_insert_with(|| ResolvedArtifact {
                    plugin_id: mount.plugin_id.clone(),
                    artifact_id: declaration.id.clone(),
                    digest: declaration.digest.clone(),
                    size: declaration.size,
                    media_type: declaration.media_type.clone(),
                    target: input.policy.value().target.clone(),
                });
            Ok(ResolvedDataMount {
                plugin_id: mount.plugin_id.clone(),
                contribution_id: mount.contribution_id.clone(),
                artifact_id: declaration.id.clone(),
                interpreter_instance_key: mount.interpreter_instance_key.clone(),
                input_slot: mount.input_slot.clone(),
                content_schema_digest: contribution.content_schema_digest.clone(),
                interpretation_schema_digest: mount.interpretation_schema_digest.clone(),
            })
        })
        .collect()
}

fn resolve_selected_artifacts(
    input: &ResolutionInput<'_>,
    artifacts: &mut BTreeMap<(String, String), ResolvedArtifact>,
) -> Result<(), ControlPlaneError> {
    for plugin in &input.lock.value().plugins {
        let manifest = manifest_for(input, &plugin.plugin_id)?;
        let selected = selected_release_content(input, &plugin.plugin_id)?;
        for artifact_id in selected.explicit_artifacts {
            let declaration = manifest
                .artifacts
                .iter()
                .find(|artifact| artifact.id == artifact_id)
                .expect("Manifest Feature references were validated at admission");
            if !declaration.targets.contains(&input.policy.value().target) {
                return Err(failed(format!(
                    "selected Artifact `{artifact_id}` does not admit target `{}`",
                    input.policy.value().target
                )));
            }
            let _ = input.store.artifact(declaration)?;
            artifacts
                .entry((plugin.plugin_id.clone(), artifact_id))
                .or_insert_with(|| ResolvedArtifact {
                    plugin_id: plugin.plugin_id.clone(),
                    artifact_id: declaration.id.clone(),
                    digest: declaration.digest.clone(),
                    size: declaration.size,
                    media_type: declaration.media_type.clone(),
                    target: input.policy.value().target.clone(),
                });
        }
    }
    Ok(())
}

fn verify_binding_template_closure(input: &ResolutionInput<'_>) -> Result<(), ControlPlaneError> {
    let instances = input
        .lock
        .value()
        .instances
        .iter()
        .map(|instance| (instance.instance_key.as_str(), instance))
        .collect::<BTreeMap<_, _>>();
    for binding in &input.bindings {
        let (Some(consumer), Some(provider)) = (
            instances.get(binding.consumer_instance()),
            instances.get(binding.provider_instance()),
        ) else {
            continue;
        };
        if consumer.plugin_id != provider.plugin_id {
            continue;
        }
        let manifest = manifest_for(input, &consumer.plugin_id)?;
        if !manifest.binding_templates.iter().any(|template| {
            template.consumer_contribution_id == consumer.contribution_id
                && template.provider_contribution_id == provider.contribution_id
                && template.capability_id == binding.capability_id()
        }) {
            return Err(failed(format!(
                "undeclared intra-Plugin binding `{}` -> `{}` for `{}`",
                binding.consumer_instance(),
                binding.provider_instance(),
                binding.capability_id()
            )));
        }
    }
    for plugin in &input.lock.value().plugins {
        let manifest = manifest_for(input, &plugin.plugin_id)?;
        for template in &manifest.binding_templates {
            let selected_consumer = input.lock.value().instances.iter().any(|instance| {
                instance.plugin_id == plugin.plugin_id
                    && instance.contribution_id == template.consumer_contribution_id
            });
            let selected_provider = input.lock.value().instances.iter().any(|instance| {
                instance.plugin_id == plugin.plugin_id
                    && instance.contribution_id == template.provider_contribution_id
            });
            if selected_consumer
                && selected_provider
                && !input.bindings.iter().any(|binding| {
                    let consumer = instances.get(binding.consumer_instance());
                    let provider = instances.get(binding.provider_instance());
                    consumer.is_some_and(|instance| {
                        instance.plugin_id == plugin.plugin_id
                            && instance.contribution_id == template.consumer_contribution_id
                    }) && provider.is_some_and(|instance| {
                        instance.plugin_id == plugin.plugin_id
                            && instance.contribution_id == template.provider_contribution_id
                    }) && binding.capability_id() == template.capability_id
                })
            {
                return Err(failed(
                    "selected intra-Plugin binding template is absent from Plan",
                ));
            }
        }
    }
    Ok(())
}

fn verify_plugin_plan_closure(
    plan: &lenso_app_plan::ResolvedAppPlan,
    resolved: &[ResolvedInstance],
) -> Result<(), ControlPlaneError> {
    for instance in resolved {
        let planned = plan
            .module_instance(&instance.instance_key)
            .ok_or_else(|| failed("resolved Plugin Instance is absent from Plan"))?;
        if planned.entrypoint() != instance.entrypoint
            || planned.execution_class().as_str() != instance.execution_class
        {
            return Err(failed(format!(
                "Plan execution input differs for Instance `{}`",
                instance.instance_key
            )));
        }
    }
    Ok(())
}

fn ensure_sorted_unique<'a>(
    values: impl IntoIterator<Item = &'a String>,
    kind: &str,
) -> Result<(), ControlPlaneError> {
    let mut previous: Option<&String> = None;
    for value in values {
        if previous.is_some_and(|previous| previous >= value) {
            return Err(failed(format!(
                "{kind} entries are not sorted and unique at `{value}`"
            )));
        }
        previous = Some(value);
    }
    Ok(())
}

fn ensure_owned_sorted_unique(values: &[String], kind: &str) -> Result<(), ControlPlaneError> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(failed(format!("{kind} entries must be sorted and unique")));
    }
    Ok(())
}

fn ensure_unique<'a>(
    values: impl IntoIterator<Item = &'a String>,
    kind: &str,
) -> Result<(), ControlPlaneError> {
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            return Err(failed(format!("duplicate {kind} `{value}`")));
        }
    }
    Ok(())
}

fn failed(detail: impl Into<String>) -> ControlPlaneError {
    ControlPlaneError::ResolutionFailed {
        detail: detail.into(),
    }
}
