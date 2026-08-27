use serde::{Deserialize, Serialize};

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
