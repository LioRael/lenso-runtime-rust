use serde::{Deserialize, Serialize};

/// Truthful location at which one effective grant is enforced.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnforcementKind {
    Capability,
    Plugin,
    Adapter,
    Host,
    TrustReviewOnly,
}

/// Immutable build identity of the executable and installed Adapter catalog.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostBuildManifest {
    pub schema_version: u32,
    pub app_id: String,
    pub host_executable_digest: String,
    pub target: String,
    pub embedded_plugins: Vec<EmbeddedPlugin>,
    pub adapter_profiles: Vec<AdapterProfile>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddedPlugin {
    pub package_id: String,
    pub factory_identity: String,
    pub execution_class: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterProfile {
    pub execution_class: String,
    pub adapter_build_identity: String,
    pub targets: Vec<String>,
    pub profiles: Vec<String>,
}

/// Product-owned deterministic execution-class selection policy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostExecutionPolicy {
    pub schema_version: u32,
    pub app_id: String,
    pub host_build_manifest_digest: String,
    pub target: String,
    pub preference: Vec<String>,
}

/// Exact artifact and implementation selection produced by resolution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedArtifactSet {
    pub schema_version: u32,
    pub resolution_authority_digest: String,
    pub host_execution_policy_digest: String,
    pub artifacts: Vec<ResolvedArtifact>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedArtifact {
    pub plugin_id: String,
    pub artifact_id: String,
    pub digest: String,
    pub size: u64,
    pub media_type: String,
    pub target: String,
}

/// Effective, named enforcement authority for selected Instances.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveHostGrantSet {
    pub schema_version: u32,
    pub resolution_authority_digest: String,
    pub grants: Vec<EffectiveGrant>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveGrant {
    pub instance_key: String,
    pub permission_request_id: String,
    pub scope: serde_json::Value,
    pub enforcement_kind: EnforcementKind,
    pub enforcer_identity: String,
    pub configuration: serde_json::Value,
}

/// Immutable identity of one complete executable App Generation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppGenerationSpec {
    pub schema_version: u32,
    pub app_id: String,
    pub host_build_manifest_digest: String,
    pub host_execution_policy_digest: String,
    pub resolved_plan_digest: String,
    pub resolution_authority_digest: String,
    pub resolved_artifact_set_digest: String,
    pub effective_host_grant_set_digest: String,
}

/// Immutable edge authority for one fenced Generation transition.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppGenerationTransitionSpec {
    pub schema_version: u32,
    pub app_id: String,
    pub from_generation_spec_digest: Option<String>,
    pub to_generation_spec_digest: String,
    pub replacement_mode: ReplacementMode,
    #[serde(default)]
    pub state_compatibility_receipt_digests: Vec<String>,
    pub rollout_policy: RolloutPolicy,
}

/// Exact runtime identity used by state replacement evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StatefulRuntimeIdentity {
    pub runtime_identity: String,
    pub state_schema_id: String,
    pub state_schema_digest: String,
}

/// Detached product decision authorizing one exact stateful replacement edge.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StateCompatibilityReceipt {
    pub schema_version: u32,
    pub app_id: String,
    pub plugin_instance_key: String,
    pub old_runtime_identity: String,
    pub new_runtime_identity: String,
    pub state_schema_id: String,
    pub old_state_schema_digest: String,
    pub new_state_schema_digest: String,
    pub compatibility: StateCompatibility,
    pub policy_digest: String,
    pub evidence_digest: String,
    pub decision_authority: String,
}

/// Exact overlap and rollback properties established by compatibility evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StateCompatibility {
    pub concurrent_read: bool,
    pub concurrent_write: bool,
    pub old_code_reads_new_writes: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplacementMode {
    Initial,
    Overlap,
    Maintenance,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RolloutPolicy {
    pub ready_timeout_nanos: String,
    pub drain_timeout_nanos: String,
    pub rollback_window_nanos: String,
    pub automatic_rollback_on_generation_failure: bool,
}
