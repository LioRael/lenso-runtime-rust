use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Publisher-owned immutable Plugin Release manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub schema_version: u32,
    pub plugin_id: String,
    pub release_version: String,
    pub artifacts: Vec<ArtifactDeclaration>,
    pub module_contributions: Vec<ModuleContribution>,
    #[serde(default)]
    pub data_contributions: Vec<DataContribution>,
    #[serde(default)]
    pub permission_requests: Vec<PermissionRequest>,
    #[serde(default)]
    pub features: Vec<PluginFeature>,
    #[serde(default)]
    pub binding_templates: Vec<BindingTemplate>,
    #[serde(default)]
    pub product_metadata: Vec<ProductMetadataDeclaration>,
}

/// Publisher-named optional set expanded before App Composition.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginFeature {
    pub id: String,
    #[serde(default)]
    pub module_contribution_ids: Vec<String>,
    #[serde(default)]
    pub data_contribution_ids: Vec<String>,
    #[serde(default)]
    pub artifact_ids: Vec<String>,
    #[serde(default)]
    pub permission_request_ids: Vec<String>,
    #[serde(default)]
    pub product_metadata_ids: Vec<String>,
}

/// Publisher-suggested binding constrained to contributions in this Release.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BindingTemplate {
    pub consumer_contribution_id: String,
    pub provider_contribution_id: String,
    pub capability_id: String,
}

/// Product-owned metadata bytes admitted generically but interpreted by the product.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProductMetadataDeclaration {
    pub id: String,
    pub namespace: String,
    pub schema_id: String,
    pub path: String,
    pub digest: String,
}

/// One exact Artifact carried by a Plugin Release.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactDeclaration {
    pub id: String,
    pub kind: ArtifactKind,
    pub digest: String,
    pub size: u64,
    pub media_type: String,
    pub path: String,
    pub targets: Vec<String>,
}

/// Admitted Artifact interpretation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Process,
    WasmComponent,
    QuickJsModule,
    NativeDylib,
    Data,
}

/// One logical Module contribution with alternative execution implementations.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleContribution {
    pub id: String,
    pub package_id: String,
    pub configuration_schema_digest: String,
    pub provides: Vec<CapabilityDeclaration>,
    #[serde(default)]
    pub requires: Vec<CapabilityRequirement>,
    pub implementations: Vec<ImplementationVariant>,
    #[serde(default)]
    pub permission_request_ids: Vec<String>,
    /// Durable state authority, absent for a stateless Module contribution.
    #[serde(default)]
    pub state: Option<StateDeclaration>,
}

/// Product-visible durable state identity owned by one logical Module contribution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StateDeclaration {
    pub state_schema_id: String,
    pub state_schema_digest: String,
}

/// Exact Capability Descriptor identity and Request Operation table.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityDeclaration {
    pub capability_id: String,
    pub descriptor_version: String,
    pub descriptor_digest: String,
    pub request_operations: Vec<String>,
}

/// One required Capability identity and cardinality.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityRequirement {
    pub capability_id: String,
    pub descriptor_version: String,
    pub cardinality: RequirementCardinality,
}

/// Plan-supported dependency cardinality.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementCardinality {
    One,
    Optional,
    Many,
}

/// One target-specific implementation of a logical Module contribution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImplementationVariant {
    pub id: String,
    pub artifact: Option<String>,
    pub built_in_factory: Option<String>,
    pub entrypoint: String,
    pub execution_class: String,
    pub targets: Vec<String>,
    pub profiles: Vec<String>,
    pub support_channel: SupportChannel,
    pub trust: TrustLevel,
}

/// Product support status admitted by host policy.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportChannel {
    Stable,
    Preview,
    Experimental,
}

/// Trust decision required by an execution implementation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    Constrained,
    Isolated,
    Trusted,
}

/// Inert Data contribution mounted into an explicit interpreter Instance.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DataContribution {
    pub id: String,
    pub artifact: String,
    pub media_type: String,
    pub content_schema_id: String,
    pub content_schema_digest: String,
    pub product_metadata_digest: String,
}

/// Publisher request which is not itself host authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionRequest {
    pub id: String,
    pub resource_kind: String,
    pub required: bool,
    pub scope: serde_json::Value,
    pub explanation_key: String,
}

/// App-local exact Plugin and Module Instance selection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSetLock {
    pub schema_version: u32,
    pub app_id: String,
    pub plugins: Vec<LockedPlugin>,
    pub instances: Vec<LockedInstance>,
    #[serde(default)]
    pub data_mounts: Vec<LockedDataMount>,
    #[serde(default)]
    pub approved_grants: Vec<ApprovedGrant>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LockedPlugin {
    pub plugin_id: String,
    pub release_version: String,
    pub manifest_digest: String,
    #[serde(default)]
    pub selected_features: Vec<String>,
    #[serde(default)]
    pub product_metadata_digests: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LockedInstance {
    pub plugin_id: String,
    pub contribution_id: String,
    pub instance_key: String,
    pub implementation_variant: Option<String>,
    pub configuration: String,
    pub execution_lane: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LockedDataMount {
    pub plugin_id: String,
    pub contribution_id: String,
    pub interpreter_instance_key: String,
    pub input_slot: String,
    pub interpretation_schema_digest: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedGrant {
    pub instance_key: String,
    pub permission_request_id: String,
    pub scope: serde_json::Value,
    pub enforcement_kind: EnforcementKind,
    pub enforcer_identity: String,
}

/// Truthful location at which one effective grant is enforced.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnforcementKind {
    Capability,
    Module,
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
    pub built_in_modules: Vec<BuiltInModule>,
    pub adapter_profiles: Vec<AdapterProfile>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuiltInModule {
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
    pub classes: Vec<ClassPolicy>,
    pub preference: Vec<String>,
    #[serde(default)]
    pub instance_overrides: Vec<InstanceExecutionOverride>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClassPolicy {
    pub execution_class: String,
    pub support_channels: Vec<SupportChannel>,
    pub trust_levels: Vec<TrustLevel>,
    pub profiles: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceExecutionOverride {
    pub instance_key: String,
    pub allowed_classes: Vec<String>,
    pub preference: Vec<String>,
}

/// Exact artifact and implementation selection produced by resolution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedArtifactSet {
    pub schema_version: u32,
    pub plugin_set_lock_digest: String,
    pub host_execution_policy_digest: String,
    pub releases: Vec<ResolvedRelease>,
    pub artifacts: Vec<ResolvedArtifact>,
    pub instances: Vec<ResolvedInstance>,
    #[serde(default)]
    pub data_mounts: Vec<ResolvedDataMount>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedRelease {
    pub plugin_id: String,
    pub release_version: String,
    pub manifest_digest: String,
    pub admission_receipt_digest: String,
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedInstance {
    pub instance_key: String,
    pub plugin_id: String,
    pub contribution_id: String,
    pub implementation_variant: String,
    pub artifact_id: Option<String>,
    pub built_in_factory: Option<String>,
    pub entrypoint: String,
    pub execution_class: String,
    pub target: String,
    pub support_channel: SupportChannel,
    pub selection_reason: String,
    pub profiles: Vec<String>,
    pub limits: BTreeMap<String, String>,
    pub provided_capabilities: Vec<CapabilityDeclaration>,
    pub required_capabilities: Vec<CapabilityRequirement>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedDataMount {
    pub plugin_id: String,
    pub contribution_id: String,
    pub artifact_id: String,
    pub interpreter_instance_key: String,
    pub input_slot: String,
    pub content_schema_digest: String,
    pub interpretation_schema_digest: String,
}

/// Effective, named enforcement authority for selected Instances.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveHostGrantSet {
    pub schema_version: u32,
    pub plugin_set_lock_digest: String,
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
    pub plugin_set_lock_digest: String,
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
    pub module_instance_key: String,
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
