use std::collections::{BTreeMap, BTreeSet};

use lenso_app_plan::PluginInstancePlan;
use lenso_runtime_codec::{ArtifactCatalog, ArtifactHandle, InstanceResourceCatalog};

use crate::sha256_digest;
use crate::{
    AppGenerationSpec, CanonicalDocument, ControlPlaneError, EffectiveHostGrantSet,
    HostBuildManifest, HostExecutionPolicy, ResolvedArtifact, ResolvedArtifactSet,
    ResolvedInstanceResources, StatefulRuntimeIdentity,
};

/// Exact output used to stage one immutable App Generation.
#[derive(Clone, Debug)]
pub struct ResolvedGeneration {
    pub plan: lenso_app_plan::ResolvedAppPlan,
    pub artifact_set: CanonicalDocument<ResolvedArtifactSet>,
    pub grants: CanonicalDocument<EffectiveHostGrantSet>,
    pub spec: CanonicalDocument<AppGenerationSpec>,
    pub artifacts: ArtifactCatalog,
    /// Exact supporting files selected for each Plugin Instance.
    pub resources: InstanceResourceCatalog,
    /// Exact stateful Instance identities used by Transition compatibility checks.
    pub stateful_instances: BTreeMap<String, StatefulRuntimeIdentity>,
}

/// One digest-verified execution Artifact already selected by a resolved App Plan.
#[derive(Clone, Debug)]
pub struct PlanArtifact {
    pub instance_key: String,
    pub plugin_id: String,
    pub artifact_id: String,
    pub media_type: String,
    pub target: String,
    pub handle: ArtifactHandle,
}

/// Minimal immutable authority required after Host + Plugin Root resolution.
#[derive(Debug)]
pub struct PlanGenerationInput<'a> {
    pub app_id: &'a str,
    pub authority_digest: &'a str,
    pub plan: &'a lenso_app_plan::ResolvedAppPlan,
    pub host_build: &'a CanonicalDocument<HostBuildManifest>,
    pub policy: &'a CanonicalDocument<HostExecutionPolicy>,
    pub artifacts: Vec<PlanArtifact>,
    pub resources: InstanceResourceCatalog,
}

/// Closes an already resolved App Plan into one executable Generation.
pub fn resolve_plan_generation(
    input: PlanGenerationInput<'_>,
) -> Result<ResolvedGeneration, ControlPlaneError> {
    if input.app_id.trim().is_empty() {
        return Err(failed("App identity cannot be empty"));
    }
    if !input.authority_digest.starts_with("sha256:") {
        return Err(failed("Plugin Root authority must be a SHA-256 digest"));
    }
    if input.host_build.value().app_id != input.app_id
        || input.policy.value().app_id != input.app_id
    {
        return Err(failed(
            "App identity differs across resolved Plan authority",
        ));
    }

    let plan_instances = input
        .plan
        .plugin_instances()
        .iter()
        .map(PluginInstancePlan::instance_key)
        .collect::<BTreeSet<_>>();
    let mut artifact_catalog = ArtifactCatalog::new();
    let mut resolved_artifacts = Vec::new();
    let mut artifact_instances = BTreeSet::new();
    for artifact in input.artifacts {
        if !plan_instances.contains(artifact.instance_key.as_str()) {
            return Err(failed(format!(
                "Artifact selects unknown Plugin Instance `{}`",
                artifact.instance_key
            )));
        }
        if !artifact_instances.insert(artifact.instance_key.clone()) {
            return Err(failed(format!(
                "duplicate Artifact authority for Plugin Instance `{}`",
                artifact.instance_key
            )));
        }
        resolved_artifacts.push(ResolvedArtifact {
            plugin_id: artifact.plugin_id,
            artifact_id: artifact.artifact_id,
            digest: artifact.handle.digest().to_owned(),
            size: artifact.handle.size(),
            media_type: artifact.media_type,
            target: artifact.target,
        });
        artifact_catalog = artifact_catalog
            .with_artifact(artifact.instance_key, artifact.handle)
            .map_err(|error| failed(format!("invalid resolved Artifact: {error:?}")))?;
    }
    resolved_artifacts.sort_by(|left, right| {
        (&left.plugin_id, &left.artifact_id).cmp(&(&right.plugin_id, &right.artifact_id))
    });
    let instance_resources = resolve_instance_resources(&input.resources, &plan_instances)?;

    let artifact_set = CanonicalDocument::from_value(
        "lenso-artifacts.lock.json",
        ResolvedArtifactSet {
            schema_version: 3,
            resolution_authority_digest: input.authority_digest.to_owned(),
            host_execution_policy_digest: input.policy.digest().to_owned(),
            artifacts: resolved_artifacts,
            instance_resources,
        },
    )?;
    let grants = CanonicalDocument::from_value(
        "lenso-host-grants.lock.json",
        EffectiveHostGrantSet {
            schema_version: 2,
            resolution_authority_digest: input.authority_digest.to_owned(),
            grants: Vec::new(),
        },
    )?;
    let plan_bytes = serde_json::to_vec(input.plan).map_err(|error| failed(error.to_string()))?;
    let spec = CanonicalDocument::from_value(
        "lenso-generation.json",
        AppGenerationSpec {
            schema_version: 2,
            app_id: input.app_id.to_owned(),
            host_build_manifest_digest: input.host_build.digest().to_owned(),
            host_execution_policy_digest: input.policy.digest().to_owned(),
            resolved_plan_digest: sha256_digest(&plan_bytes),
            resolution_authority_digest: input.authority_digest.to_owned(),
            resolved_artifact_set_digest: artifact_set.digest().to_owned(),
            effective_host_grant_set_digest: grants.digest().to_owned(),
        },
    )?;
    Ok(ResolvedGeneration {
        plan: input.plan.clone(),
        artifact_set,
        grants,
        spec,
        artifacts: artifact_catalog,
        resources: input.resources,
        stateful_instances: BTreeMap::new(),
    })
}

fn resolve_instance_resources(
    catalog: &InstanceResourceCatalog,
    plan_instances: &BTreeSet<&str>,
) -> Result<Vec<ResolvedInstanceResources>, ControlPlaneError> {
    catalog
        .iter()
        .map(|(instance_key, resources)| {
            if !plan_instances.contains(instance_key) {
                return Err(failed(format!(
                    "resources select unknown Plugin Instance `{instance_key}`"
                )));
            }
            Ok(ResolvedInstanceResources {
                instance_key: instance_key.to_owned(),
                digest: resources.digest().to_owned(),
                file_count: u64::try_from(resources.file_count())
                    .map_err(|_| failed("resource file count exceeds u64"))?,
                total_size: resources.total_size(),
            })
        })
        .collect()
}

fn failed(detail: impl Into<String>) -> ControlPlaneError {
    ControlPlaneError::ResolutionFailed {
        detail: detail.into(),
    }
}
