use std::collections::BTreeMap;

use lenso_app_plan::CapabilityOperationKind;
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

/// Generated one-entry Plugin Manifest produced from source evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifestV2 {
    pub schema_version: u32,
    pub plugin_id: String,
    pub release_version: String,
    pub artifact: PluginArtifactV2,
    pub entry: PluginEntryV2,
}

/// Exact final Component facts computed by the Bundle builder.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginArtifactV2 {
    pub path: String,
    pub digest: String,
    pub size: u64,
    pub media_type: String,
    pub target: String,
}

/// The single executable Plugin entry derived from source.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginEntryV2 {
    pub descriptor: serde_json::Value,
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

/// Exact Capability Descriptor identity and Operation interaction table.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityDeclaration {
    pub capability_id: String,
    pub descriptor_version: String,
    pub descriptor_digest: String,
    /// Historical field name for the complete ordered Operation table.
    pub request_operations: Vec<String>,
    /// Non-Request interaction kinds; omitted Operations default to Request.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub operation_kinds: BTreeMap<String, CapabilityOperationKind>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_request_capability_declaration_remains_canonical() {
        let bytes = br#"{"capability_id":"example.echo@1","descriptor_version":"1.0.0","descriptor_digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","request_operations":["echo"]}"#;
        let declaration: CapabilityDeclaration = serde_json::from_slice(bytes).unwrap();

        assert!(declaration.operation_kinds.is_empty());
        assert_eq!(serde_json::to_vec(&declaration).unwrap(), bytes);
    }
}
