use serde::{Deserialize, Serialize};

use lenso_app_plan::authoring::{PluginContract, PluginImplementation};

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

/// One Plugin Release contract with every publisher-provided implementation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifestV3 {
    pub schema_version: u32,
    pub contract: PluginContract,
    pub implementations: Vec<PluginImplementationV3>,
}

/// One exact executable implementation of a V3 Plugin contract.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginImplementationV3 {
    pub id: String,
    pub host_targets: Vec<String>,
    pub artifact: PluginArtifactV2,
    pub runtime: PluginImplementation,
}

/// A strictly parsed Plugin Manifest, including the legacy single-artifact form.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginManifest {
    V2(PluginManifestV2),
    V3(PluginManifestV3),
}

impl PluginManifest {
    pub fn plugin_id(&self) -> &str {
        match self {
            Self::V2(value) => &value.plugin_id,
            Self::V3(value) => value.contract.plugin_id(),
        }
    }

    pub fn release_version(&self) -> &str {
        match self {
            Self::V2(value) => &value.release_version,
            Self::V3(value) => value.contract.release_version(),
        }
    }
}
