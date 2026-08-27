//! Immutable Plugin Release manifests, source materialization, and Bundle verification.

mod model;

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::{Component, Path, PathBuf},
};

pub use model::*;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The only manifest filename accepted in a materialized Plugin Bundle.
pub const MANIFEST_FILE: &str = "lenso-plugin.json";

/// Custom section carrying source-derived Plugin descriptor bytes.
pub const PLUGIN_DESCRIPTOR_SECTION: &str = "lenso.plugin-descriptor.v1";

/// Maximum accepted source-derived descriptor size.
pub const MAX_PLUGIN_DESCRIPTOR_BYTES: usize = 64 * 1024;

/// A source Artifact selected while materializing one Release.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactSource {
    pub artifact_id: String,
    pub path: PathBuf,
}

/// Exact input to one fail-closed Bundle build.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BundleBuild {
    pub template: PathBuf,
    pub output: PathBuf,
    pub artifact_sources: Vec<ArtifactSource>,
}

/// Source-only input for one generated V2 Plugin Bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourcePluginBuild {
    pub package_manifest: PathBuf,
    pub wasm_module: PathBuf,
    pub output: PathBuf,
}

/// Canonical Plugin Manifest plus its content identity.
#[derive(Clone, Debug)]
pub struct ManifestDocument {
    value: PluginManifest,
    bytes: Vec<u8>,
    digest: String,
}

impl ManifestDocument {
    /// Strictly parses and validates one publisher Manifest.
    pub fn parse(input: &[u8]) -> Result<Self, BundleError> {
        let value = strict_json::<PluginManifest>(input)?;
        Self::from_value(value)
    }

    /// Validates and canonicalizes one typed publisher Manifest.
    pub fn from_value(value: PluginManifest) -> Result<Self, BundleError> {
        validate_manifest(&value)?;
        let json = serde_json::to_value(&value)
            .map_err(|error| BundleError::InvalidManifest(error.to_string()))?;
        validate_json_value(&json)?;
        let bytes = serde_json::to_vec(&json)
            .map_err(|error| BundleError::InvalidManifest(error.to_string()))?;
        let digest = sha256_digest(&bytes);
        Ok(Self {
            value,
            bytes,
            digest,
        })
    }

    pub const fn value(&self) -> &PluginManifest {
        &self.value
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }
}

#[derive(Clone, Debug)]
struct SourceManifestDocument {
    value: PluginManifestV2,
    bytes: Vec<u8>,
    digest: String,
}

impl SourceManifestDocument {
    fn parse(input: &[u8]) -> Result<Self, BundleError> {
        let value = strict_json::<PluginManifestV2>(input)?;
        Self::from_value(value)
    }

    fn from_value(value: PluginManifestV2) -> Result<Self, BundleError> {
        validate_source_manifest(&value)?;
        let json = serde_json::to_value(&value)
            .map_err(|error| BundleError::InvalidManifest(error.to_string()))?;
        validate_json_value(&json)?;
        let bytes = serde_json::to_vec(&json)
            .map_err(|error| BundleError::InvalidManifest(error.to_string()))?;
        let digest = sha256_digest(&bytes);
        Ok(Self {
            value,
            bytes,
            digest,
        })
    }
}

#[derive(Debug, Deserialize)]
struct CargoManifest {
    package: CargoPackage,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    version: String,
    metadata: CargoMetadata,
}

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    lenso: CargoLensoMetadata,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct CargoLensoMetadata {
    plugin_id: String,
}

/// Verified closure of one immutable Plugin Release.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedBundle {
    pub plugin_id: String,
    pub release_version: String,
    pub manifest_digest: String,
    pub artifact_digests: Vec<String>,
    pub product_metadata_digests: Vec<String>,
}

/// A Plugin authoring or immutable Bundle invariant failed closed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BundleError {
    InvalidManifest(String),
    InvalidBundle(String),
    DigestMismatch(String),
    Io(String),
    Wasm(String),
}

impl fmt::Display for BundleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidManifest(detail) => write!(formatter, "invalid Plugin Manifest: {detail}"),
            Self::InvalidBundle(detail) => write!(formatter, "invalid Plugin Bundle: {detail}"),
            Self::DigestMismatch(subject) => write!(formatter, "digest mismatch for {subject}"),
            Self::Io(detail) => formatter.write_str(detail),
            Self::Wasm(detail) => write!(
                formatter,
                "failed to encode WebAssembly Component: {detail}"
            ),
        }
    }
}

impl std::error::Error for BundleError {}

/// Builds a one-entry V2 Plugin Bundle entirely from package and source evidence.
pub fn build_source_plugin_bundle(
    build: &SourcePluginBuild,
) -> Result<VerifiedBundle, BundleError> {
    if build.output.exists() {
        return invalid_bundle(format!(
            "output `{}` already exists",
            build.output.display()
        ));
    }
    let package_bytes = read_regular_file(&build.package_manifest, "Cargo manifest")?;
    let package = toml::from_slice::<CargoManifest>(&package_bytes)
        .map_err(|error| BundleError::InvalidManifest(error.to_string()))?;
    let module = read_regular_file(&build.wasm_module, "Plugin Wasm module")?;
    let component = wit_component::ComponentEncoder::default()
        .module(&module)
        .map_err(|error| BundleError::Wasm(error.to_string()))?
        .validate(true)
        .encode()
        .map_err(|error| BundleError::Wasm(error.to_string()))?;
    let descriptor = extract_plugin_descriptor(&component)?;
    let descriptor = strict_json::<Value>(&descriptor)?;
    let artifact = PluginArtifactV2 {
        path: "plugin.wasm".to_owned(),
        digest: sha256_digest(&component),
        size: u64::try_from(component.len())
            .map_err(|_| BundleError::InvalidBundle("Artifact size exceeds u64".to_owned()))?,
        media_type: "application/wasm".to_owned(),
        target: "wasm32-unknown-unknown".to_owned(),
    };
    let document = SourceManifestDocument::from_value(PluginManifestV2 {
        schema_version: 2,
        plugin_id: package.package.metadata.lenso.plugin_id,
        release_version: package.package.version,
        artifact,
        entry: PluginEntryV2 { descriptor },
    })?;

    let output_parent = build.output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(output_parent).map_err(io_error)?;
    let staging = tempfile::Builder::new()
        .prefix(".lenso-plugin-")
        .tempdir_in(output_parent)
        .map_err(io_error)?;
    write_bundle_file(staging.path(), &document.value.artifact.path, &component)?;
    fs::write(staging.path().join(MANIFEST_FILE), &document.bytes).map_err(io_error)?;
    fs::rename(staging.path(), &build.output).map_err(io_error)?;
    verify_bundle_directory(&build.output)
}

/// Builds an immutable Bundle directory without executing publisher code.
///
/// This is the schema-version-1 compatibility creator. New Plugin authoring
/// uses [`build_source_plugin_bundle`].
pub fn build_bundle(build: &BundleBuild) -> Result<VerifiedBundle, BundleError> {
    if build.output.exists() {
        return invalid_bundle(format!(
            "output `{}` already exists",
            build.output.display()
        ));
    }
    let template = read_regular_file(&build.template, "Plugin Manifest template")?;
    let mut manifest = ManifestDocument::parse(&template)?.value;
    let template_root = build.template.parent().unwrap_or_else(|| Path::new("."));
    let sources = build
        .artifact_sources
        .iter()
        .map(|source| (source.artifact_id.as_str(), source.path.as_path()))
        .collect::<BTreeMap<_, _>>();
    if sources.len() != build.artifact_sources.len() {
        return invalid_bundle("Artifact source IDs must be unique");
    }
    if sources
        .keys()
        .any(|id| !manifest.artifacts.iter().any(|artifact| artifact.id == *id))
    {
        return invalid_bundle("Artifact source references an unknown Artifact ID");
    }

    let output_parent = build.output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(output_parent).map_err(io_error)?;
    let staging = tempfile::Builder::new()
        .prefix(".lenso-plugin-")
        .tempdir_in(output_parent)
        .map_err(io_error)?;
    let mut files = BTreeMap::new();
    for artifact in &mut manifest.artifacts {
        validate_relative_path(&artifact.path)?;
        let source = sources
            .get(artifact.id.as_str())
            .copied()
            .map_or_else(|| template_root.join(&artifact.path), Path::to_path_buf);
        let source_bytes = read_regular_file(&source, &format!("Artifact `{}`", artifact.id))?;
        let bytes = if artifact.kind == ArtifactKind::WasmComponent {
            wit_component::ComponentEncoder::default()
                .module(&source_bytes)
                .map_err(|error| BundleError::Wasm(error.to_string()))?
                .validate(true)
                .encode()
                .map_err(|error| BundleError::Wasm(error.to_string()))?
        } else {
            source_bytes
        };
        artifact.digest = sha256_digest(&bytes);
        artifact.size = u64::try_from(bytes.len())
            .map_err(|_| BundleError::InvalidBundle("Artifact size exceeds u64".to_owned()))?;
        write_bundle_file(staging.path(), &artifact.path, &bytes)?;
        files.insert(artifact.path.clone(), bytes);
    }
    for metadata in &mut manifest.product_metadata {
        validate_relative_path(&metadata.path)?;
        let bytes = read_regular_file(
            &template_root.join(&metadata.path),
            &format!("Product Metadata `{}`", metadata.id),
        )?;
        metadata.digest = sha256_digest(&bytes);
        write_bundle_file(staging.path(), &metadata.path, &bytes)?;
        files.insert(metadata.path.clone(), bytes);
    }

    let document = ManifestDocument::from_value(manifest)?;
    fs::write(staging.path().join(MANIFEST_FILE), document.bytes()).map_err(io_error)?;
    let verified = verify_bundle_files(&document, &files)?;
    fs::rename(staging.path(), &build.output).map_err(io_error)?;
    Ok(verified)
}

/// Verifies an already materialized directory as an exact immutable Bundle closure.
pub fn verify_bundle_directory(root: &Path) -> Result<VerifiedBundle, BundleError> {
    let manifest_path = root.join(MANIFEST_FILE);
    let manifest_bytes = read_regular_file(&manifest_path, "Plugin Manifest")?;
    let mut files = BTreeMap::new();
    collect_bundle_files(root, root, &mut files)?;
    files.remove(MANIFEST_FILE);
    match manifest_schema_version(&manifest_bytes)? {
        1 => verify_bundle_files(&ManifestDocument::parse(&manifest_bytes)?, &files),
        2 => verify_source_bundle_files(&SourceManifestDocument::parse(&manifest_bytes)?, &files),
        _ => invalid_manifest("unsupported schema version"),
    }
}

/// Verifies exact declared bytes without performing admission or granting authority.
pub fn verify_bundle_files(
    manifest: &ManifestDocument,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<VerifiedBundle, BundleError> {
    let declared_paths = manifest
        .value()
        .artifacts
        .iter()
        .map(|artifact| artifact.path.as_str())
        .chain(
            manifest
                .value()
                .product_metadata
                .iter()
                .map(|metadata| metadata.path.as_str()),
        )
        .collect::<BTreeSet<_>>();
    if declared_paths.len()
        != manifest.value().artifacts.len() + manifest.value().product_metadata.len()
    {
        return invalid_bundle("Manifest declares a duplicate Bundle path");
    }
    if files.len() != declared_paths.len()
        || files
            .keys()
            .any(|path| !declared_paths.contains(path.as_str()))
    {
        return invalid_bundle("Bundle files do not exactly close over declared paths");
    }
    let mut artifact_digests = Vec::new();
    for artifact in &manifest.value().artifacts {
        let bytes = files.get(&artifact.path).ok_or_else(|| {
            BundleError::InvalidBundle(format!("missing Artifact `{}`", artifact.id))
        })?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != artifact.size {
            return invalid_bundle(format!("Artifact `{}` size does not match", artifact.id));
        }
        if sha256_digest(bytes) != artifact.digest {
            return Err(BundleError::DigestMismatch(format!(
                "Artifact `{}`",
                artifact.id
            )));
        }
        artifact_digests.push(artifact.digest.clone());
    }
    artifact_digests.sort();
    artifact_digests.dedup();
    let mut product_metadata_digests = Vec::new();
    for metadata in &manifest.value().product_metadata {
        let bytes = files.get(&metadata.path).ok_or_else(|| {
            BundleError::InvalidBundle(format!("missing Product Metadata `{}`", metadata.id))
        })?;
        if sha256_digest(bytes) != metadata.digest {
            return Err(BundleError::DigestMismatch(format!(
                "Product Metadata `{}`",
                metadata.id
            )));
        }
        product_metadata_digests.push(metadata.digest.clone());
    }
    product_metadata_digests.sort();
    product_metadata_digests.dedup();
    Ok(VerifiedBundle {
        plugin_id: manifest.value().plugin_id.clone(),
        release_version: manifest.value().release_version.clone(),
        manifest_digest: manifest.digest().to_owned(),
        artifact_digests,
        product_metadata_digests,
    })
}

fn verify_source_bundle_files(
    manifest: &SourceManifestDocument,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<VerifiedBundle, BundleError> {
    let artifact = &manifest.value.artifact;
    if files.len() != 1 {
        return invalid_bundle("V2 Bundle must contain exactly one Artifact");
    }
    let Some(bytes) = files.get(&artifact.path) else {
        return invalid_bundle("V2 Bundle does not contain its declared Artifact");
    };
    if artifact.size != u64::try_from(bytes.len()).unwrap_or(u64::MAX)
        || artifact.digest != sha256_digest(bytes)
    {
        return Err(BundleError::DigestMismatch(artifact.path.clone()));
    }
    let descriptor = extract_plugin_descriptor(bytes)?;
    let packaged = serde_json::to_vec(&manifest.value.entry.descriptor)
        .map_err(|error| BundleError::InvalidManifest(error.to_string()))?;
    if descriptor != packaged {
        return invalid_bundle("source descriptor does not match the V2 Plugin entry");
    }
    Ok(VerifiedBundle {
        plugin_id: manifest.value.plugin_id.clone(),
        release_version: manifest.value.release_version.clone(),
        manifest_digest: manifest.digest.clone(),
        artifact_digests: vec![artifact.digest.clone()],
        product_metadata_digests: Vec::new(),
    })
}

fn manifest_schema_version(input: &[u8]) -> Result<u64, BundleError> {
    let value = strict_json::<Value>(input)?;
    value
        .get("schema_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| BundleError::InvalidManifest("schema_version is required".to_owned()))
}

/// Extracts one canonical source-derived Plugin descriptor without executing it.
pub fn extract_plugin_descriptor(component: &[u8]) -> Result<Vec<u8>, BundleError> {
    let mut descriptors = Vec::new();
    collect_plugin_descriptors(component, &mut descriptors)?;
    let [descriptor] = descriptors.as_slice() else {
        return invalid_bundle(if descriptors.is_empty() {
            "Plugin Component does not contain a source-derived descriptor"
        } else {
            "Plugin Component contains duplicate source-derived descriptors"
        });
    };
    let value = strict_json::<Value>(descriptor)?;
    let canonical = serde_json::to_vec(&value)
        .map_err(|error| BundleError::InvalidManifest(error.to_string()))?;
    if canonical != *descriptor {
        return invalid_bundle("Plugin descriptor is not canonical JSON");
    }
    Ok(descriptor.clone())
}

fn collect_plugin_descriptors(
    bytes: &[u8],
    descriptors: &mut Vec<Vec<u8>>,
) -> Result<(), BundleError> {
    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        match payload.map_err(|error| BundleError::Wasm(error.to_string()))? {
            wasmparser::Payload::CustomSection(section)
                if section.name() == PLUGIN_DESCRIPTOR_SECTION =>
            {
                if section.data().len() > MAX_PLUGIN_DESCRIPTOR_BYTES {
                    return invalid_bundle("Plugin descriptor exceeds the size limit");
                }
                descriptors.push(section.data().to_vec());
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_source_manifest(manifest: &PluginManifestV2) -> Result<(), BundleError> {
    if manifest.schema_version != 2 {
        return invalid_manifest("unsupported schema version");
    }
    if manifest.plugin_id.is_empty() || semver::Version::parse(&manifest.release_version).is_err() {
        return invalid_manifest("Plugin identity or Release version is invalid");
    }
    validate_relative_path(&manifest.artifact.path)?;
    digest_component(&manifest.artifact.digest)?;
    if manifest.artifact.size == 0
        || manifest.artifact.media_type != "application/wasm"
        || manifest.artifact.target != "wasm32-unknown-unknown"
    {
        return invalid_manifest("V2 Artifact size, Wasm media type, and target must be exact");
    }
    if !manifest.entry.descriptor.is_object() {
        return invalid_manifest("V2 Plugin entry descriptor must be an object");
    }
    Ok(())
}

/// Validates publisher-owned Manifest semantics independently of Host policy.
#[allow(clippy::too_many_lines)]
pub fn validate_manifest(manifest: &PluginManifest) -> Result<(), BundleError> {
    if manifest.schema_version != 1 {
        return invalid_manifest("unsupported schema version");
    }
    if manifest.plugin_id.is_empty() || semver::Version::parse(&manifest.release_version).is_err() {
        return invalid_manifest("Plugin identity or Release version is invalid");
    }
    let artifact_ids = manifest
        .artifacts
        .iter()
        .map(|artifact| artifact.id.as_str())
        .collect::<Vec<_>>();
    let contribution_ids = manifest
        .module_contributions
        .iter()
        .map(|contribution| contribution.id.as_str())
        .collect::<Vec<_>>();
    let data_ids = manifest
        .data_contributions
        .iter()
        .map(|contribution| contribution.id.as_str())
        .collect::<Vec<_>>();
    let permission_ids = manifest
        .permission_requests
        .iter()
        .map(|request| request.id.as_str())
        .collect::<Vec<_>>();
    let feature_ids = manifest
        .features
        .iter()
        .map(|feature| feature.id.as_str())
        .collect::<Vec<_>>();
    let metadata_ids = manifest
        .product_metadata
        .iter()
        .map(|metadata| metadata.id.as_str())
        .collect::<Vec<_>>();
    ensure_sorted_unique(&artifact_ids, "Artifact ID")?;
    ensure_sorted_unique(&contribution_ids, "Module contribution ID")?;
    ensure_sorted_unique(&data_ids, "Data contribution ID")?;
    ensure_sorted_unique(&permission_ids, "permission request ID")?;
    ensure_sorted_unique(&feature_ids, "Feature ID")?;
    ensure_sorted_unique(&metadata_ids, "Product Metadata ID")?;
    let artifact_set = artifact_ids.iter().copied().collect::<BTreeSet<_>>();
    let contribution_set = contribution_ids.iter().copied().collect::<BTreeSet<_>>();
    let data_set = data_ids.iter().copied().collect::<BTreeSet<_>>();
    let permission_set = permission_ids.iter().copied().collect::<BTreeSet<_>>();
    let metadata_set = metadata_ids.iter().copied().collect::<BTreeSet<_>>();
    for artifact in &manifest.artifacts {
        digest_component(&artifact.digest)?;
        validate_relative_path(&artifact.path)?;
        if artifact.id.is_empty() || artifact.media_type.is_empty() || artifact.targets.is_empty() {
            return invalid_manifest("Artifact identity, media type, and targets must be explicit");
        }
        ensure_sorted_unique(&artifact.targets, "Artifact target")?;
    }
    for contribution in &manifest.module_contributions {
        digest_component(&contribution.configuration_schema_digest)?;
        if contribution.id.is_empty()
            || contribution.package_id.is_empty()
            || contribution.implementations.is_empty()
        {
            return invalid_manifest(
                "Module contribution identity and implementations are required",
            );
        }
        let provided = contribution
            .provides
            .iter()
            .map(|capability| capability.capability_id.as_str())
            .collect::<Vec<_>>();
        ensure_sorted_unique(&provided, "provided Capability")?;
        for capability in &contribution.provides {
            digest_component(&capability.descriptor_digest)?;
            validate_semver(
                &capability.descriptor_version,
                "Capability Descriptor version",
            )?;
            ensure_sorted_unique(&capability.request_operations, "Capability Operation")?;
            if capability
                .operation_kinds
                .keys()
                .any(|operation| !capability.request_operations.contains(operation))
            {
                return invalid_manifest(
                    "Capability interaction kind references an unknown Operation",
                );
            }
        }
        let required = contribution
            .requires
            .iter()
            .map(|capability| capability.capability_id.as_str())
            .collect::<Vec<_>>();
        ensure_sorted_unique(&required, "required Capability")?;
        for capability in &contribution.requires {
            validate_semver(
                &capability.descriptor_version,
                "required Capability Descriptor version",
            )?;
        }
        ensure_sorted_unique(
            &contribution.permission_request_ids,
            "Module permission request",
        )?;
        if contribution
            .permission_request_ids
            .iter()
            .any(|request| !permission_set.contains(request.as_str()))
        {
            return invalid_manifest(
                "Module contribution references an unknown permission request",
            );
        }
        if let Some(state) = &contribution.state {
            if state.state_schema_id.is_empty() {
                return invalid_manifest("state Schema identity is empty");
            }
            digest_component(&state.state_schema_digest)?;
        }
        let variants = contribution
            .implementations
            .iter()
            .map(|variant| variant.id.as_str())
            .collect::<Vec<_>>();
        ensure_sorted_unique(&variants, "implementation variant ID")?;
        for variant in &contribution.implementations {
            if variant.artifact.is_some() == variant.built_in_factory.is_some() {
                return invalid_manifest("implementation must select exactly one execution input");
            }
            if variant.built_in_factory.is_some()
                && variant.execution_class != "lenso.native-rust@1"
            {
                return invalid_manifest("only native Rust may select a built-in factory");
            }
            if variant
                .artifact
                .as_deref()
                .is_some_and(|artifact| !artifact_set.contains(artifact))
            {
                return invalid_manifest("implementation references an unknown Artifact");
            }
            if variant.entrypoint.is_empty()
                || variant.execution_class.is_empty()
                || variant.targets.is_empty()
                || variant.profiles.is_empty()
            {
                return invalid_manifest(
                    "implementation entrypoint, class, targets, and Profiles are required",
                );
            }
            ensure_sorted_unique(&variant.targets, "implementation target")?;
            ensure_sorted_unique(&variant.profiles, "implementation Profile")?;
        }
    }
    for contribution in &manifest.data_contributions {
        if !artifact_set.contains(contribution.artifact.as_str())
            || contribution.media_type.is_empty()
            || contribution.content_schema_id.is_empty()
        {
            return invalid_manifest("Data contribution references incomplete authority");
        }
        digest_component(&contribution.content_schema_digest)?;
        digest_component(&contribution.product_metadata_digest)?;
    }
    for request in &manifest.permission_requests {
        if request.id.is_empty()
            || request.resource_kind.is_empty()
            || request.explanation_key.is_empty()
        {
            return invalid_manifest("permission request identity is incomplete");
        }
    }
    for metadata in &manifest.product_metadata {
        if metadata.id.is_empty() || metadata.namespace.is_empty() || metadata.schema_id.is_empty()
        {
            return invalid_manifest("Product Metadata identity is incomplete");
        }
        validate_relative_path(&metadata.path)?;
        digest_component(&metadata.digest)?;
    }
    for feature in &manifest.features {
        validate_refs(
            &feature.module_contribution_ids,
            &contribution_set,
            "Module contribution",
        )?;
        validate_refs(
            &feature.data_contribution_ids,
            &data_set,
            "Data contribution",
        )?;
        validate_refs(&feature.artifact_ids, &artifact_set, "Artifact")?;
        validate_refs(
            &feature.permission_request_ids,
            &permission_set,
            "permission request",
        )?;
        validate_refs(
            &feature.product_metadata_ids,
            &metadata_set,
            "Product Metadata",
        )?;
    }
    for template in &manifest.binding_templates {
        if !contribution_set.contains(template.consumer_contribution_id.as_str())
            || !contribution_set.contains(template.provider_contribution_id.as_str())
            || template.capability_id.is_empty()
        {
            return invalid_manifest("binding template references unknown contribution authority");
        }
        let consumer = manifest
            .module_contributions
            .iter()
            .find(|value| value.id == template.consumer_contribution_id)
            .expect("validated contribution");
        let provider = manifest
            .module_contributions
            .iter()
            .find(|value| value.id == template.provider_contribution_id)
            .expect("validated contribution");
        if !consumer
            .requires
            .iter()
            .any(|value| value.capability_id == template.capability_id)
            || !provider
                .provides
                .iter()
                .any(|value| value.capability_id == template.capability_id)
        {
            return invalid_manifest("binding template Capability is not required and provided");
        }
    }
    Ok(())
}

/// Computes the canonical digest syntax used by Plugin Release documents and files.
pub fn sha256_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn strict_json<T: DeserializeOwned>(input: &[u8]) -> Result<T, BundleError> {
    let mut deserializer = serde_json::Deserializer::from_slice(input);
    let strict = StrictValue::deserialize(&mut deserializer)
        .map_err(|error| BundleError::InvalidManifest(error.to_string()))?;
    deserializer
        .end()
        .map_err(|error| BundleError::InvalidManifest(error.to_string()))?;
    validate_json_value(&strict.0)?;
    serde_json::from_value(strict.0)
        .map_err(|error| BundleError::InvalidManifest(error.to_string()))
}

#[derive(Clone, Debug)]
struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> serde::de::Visitor<'de> for StrictVisitor {
    type Value = StrictValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("strict Plugin Manifest JSON")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Bool(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(value.into())))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        u64::try_from(value)
            .map_err(|_| E::custom("negative integers are forbidden"))
            .and_then(|value| self.visit_u64(value))
    }

    fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Err(E::custom("floating-point values are forbidden"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<StrictValue>()? {
            values.push(value.0);
        }
        Ok(StrictValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        let mut keys = BTreeSet::new();
        let mut values = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(serde::de::Error::custom(format!("duplicate field `{key}`")));
            }
            values.insert(key, map.next_value::<StrictValue>()?.0);
        }
        Ok(StrictValue(Value::Object(values)))
    }
}

fn validate_json_value(value: &Value) -> Result<(), BundleError> {
    match value {
        Value::Number(number) if !number.is_u64() => {
            invalid_manifest("numbers must be non-negative integers")
        }
        Value::Array(values) => values.iter().try_for_each(validate_json_value),
        Value::Object(values) => values.values().try_for_each(validate_json_value),
        _ => Ok(()),
    }
}

fn validate_relative_path(path: &str) -> Result<(), BundleError> {
    if path.is_empty() || path.contains('\\') {
        return invalid_manifest("Bundle path is empty or platform-ambiguous");
    }
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return invalid_manifest("Bundle path must contain only normalized relative segments");
    }
    Ok(())
}

fn digest_component(digest: &str) -> Result<&str, BundleError> {
    let Some(value) = digest.strip_prefix("sha256:") else {
        return invalid_manifest("digest does not use sha256 prefix");
    };
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return invalid_manifest("digest is not 64 lowercase hexadecimal characters");
    }
    Ok(value)
}

fn validate_semver(value: &str, kind: &str) -> Result<(), BundleError> {
    semver::Version::parse(value)
        .map(|_| ())
        .map_err(|_| BundleError::InvalidManifest(format!("{kind} is not SemVer")))
}

fn ensure_sorted_unique<T: Ord + fmt::Display>(
    values: &[T],
    kind: &str,
) -> Result<(), BundleError> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return invalid_manifest(format!("{kind} entries must be sorted and unique"));
    }
    Ok(())
}

fn validate_refs<'a>(
    values: &'a [String],
    known: &BTreeSet<&'a str>,
    kind: &str,
) -> Result<(), BundleError> {
    ensure_sorted_unique(values, kind)?;
    if values.iter().any(|value| !known.contains(value.as_str())) {
        return invalid_manifest(format!("Feature references an unknown {kind}"));
    }
    Ok(())
}

fn read_regular_file(path: &Path, kind: &str) -> Result<Vec<u8>, BundleError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| BundleError::Io(format!("failed to inspect {kind}: {error}")))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return invalid_bundle(format!("{kind} is not a regular file"));
    }
    fs::read(path).map_err(io_error)
}

fn write_bundle_file(root: &Path, relative: &str, bytes: &[u8]) -> Result<(), BundleError> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    fs::write(path, bytes).map_err(io_error)
}

fn collect_bundle_files(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(), BundleError> {
    let metadata = fs::symlink_metadata(directory).map_err(io_error)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return invalid_bundle("Bundle root contains a non-regular directory");
    }
    let mut entries = fs::read_dir(directory)
        .map_err(io_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(io_error)?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(io_error)?;
        if metadata.file_type().is_symlink() {
            return invalid_bundle("Bundle contains a symbolic link");
        }
        if metadata.is_dir() {
            collect_bundle_files(root, &path, files)?;
            continue;
        }
        if !metadata.is_file() {
            return invalid_bundle("Bundle contains a non-regular file");
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| BundleError::InvalidBundle("Bundle path escaped root".to_owned()))?
            .to_str()
            .ok_or_else(|| BundleError::InvalidBundle("Bundle path is not UTF-8".to_owned()))?
            .replace(std::path::MAIN_SEPARATOR, "/");
        validate_relative_path(&relative)?;
        files.insert(relative, fs::read(path).map_err(io_error)?);
    }
    Ok(())
}

fn invalid_manifest<T>(detail: impl Into<String>) -> Result<T, BundleError> {
    Err(BundleError::InvalidManifest(detail.into()))
}

fn invalid_bundle<T>(detail: impl Into<String>) -> Result<T, BundleError> {
    Err(BundleError::InvalidBundle(detail.into()))
}

fn io_error(error: impl fmt::Display) -> BundleError {
    BundleError::Io(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;

    const ZERO_DIGEST: &str =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    fn template() -> PluginManifest {
        PluginManifest {
            schema_version: 1,
            plugin_id: "example.quickjs".to_owned(),
            release_version: "1.0.0".to_owned(),
            artifacts: vec![ArtifactDeclaration {
                id: "script".to_owned(),
                kind: ArtifactKind::QuickJsModule,
                digest: ZERO_DIGEST.to_owned(),
                size: 0,
                media_type: "text/javascript".to_owned(),
                path: "plugin.mjs".to_owned(),
                targets: vec!["aarch64-macos".to_owned()],
            }],
            module_contributions: Vec::new(),
            data_contributions: Vec::new(),
            permission_requests: Vec::new(),
            features: Vec::new(),
            binding_templates: Vec::new(),
            product_metadata: Vec::new(),
        }
    }

    fn wasm_template_with_duplicated_descriptor() -> PluginManifest {
        PluginManifest {
            schema_version: 1,
            plugin_id: "example.echo".to_owned(),
            release_version: "1.0.0".to_owned(),
            artifacts: vec![ArtifactDeclaration {
                id: "guest".to_owned(),
                kind: ArtifactKind::WasmComponent,
                digest: ZERO_DIGEST.to_owned(),
                size: 0,
                media_type: "application/wasm".to_owned(),
                path: "plugin.wasm".to_owned(),
                targets: vec!["wasm32-unknown-unknown".to_owned()],
            }],
            module_contributions: vec![ModuleContribution {
                id: "echo".to_owned(),
                package_id: "example.echo".to_owned(),
                configuration_schema_digest: ZERO_DIGEST.to_owned(),
                provides: vec![CapabilityDeclaration {
                    capability_id: "test.echo@1".to_owned(),
                    descriptor_version: "1.0.0".to_owned(),
                    descriptor_digest: ZERO_DIGEST.to_owned(),
                    request_operations: vec!["echo".to_owned()],
                    operation_kinds: BTreeMap::new(),
                }],
                requires: Vec::new(),
                implementations: vec![ImplementationVariant {
                    id: "wasm".to_owned(),
                    artifact: Some("guest".to_owned()),
                    built_in_factory: None,
                    entrypoint: "echo".to_owned(),
                    execution_class: "lenso.wasm-component@1".to_owned(),
                    targets: vec!["wasm32-unknown-unknown".to_owned()],
                    profiles: vec!["provide-request-v1".to_owned()],
                    support_channel: SupportChannel::Preview,
                    trust: TrustLevel::Constrained,
                }],
                permission_request_ids: Vec::new(),
                state: None,
            }],
            data_contributions: Vec::new(),
            permission_requests: Vec::new(),
            features: Vec::new(),
            binding_templates: Vec::new(),
            product_metadata: Vec::new(),
        }
    }

    fn wasm_with_descriptors(descriptors: &[&[u8]]) -> Vec<u8> {
        let mut module = wasm_encoder::Module::new();
        for descriptor in descriptors {
            module.section(&wasm_encoder::CustomSection {
                name: Cow::Borrowed(PLUGIN_DESCRIPTOR_SECTION),
                data: Cow::Borrowed(descriptor),
            });
        }
        module.finish()
    }

    #[test]
    fn publisher_template_duplicates_guest_descriptor() {
        let guest_descriptor = lenso_guest_sdk::encode_guest_descriptor(
            &[lenso_guest_sdk::GuestProvidedCapability {
                capability_id: "test.echo@1",
                descriptor_version: "1.0.0",
                request_operations: &["echo"],
                stream_operations: &[],
            }],
            &[],
        );
        let guest: Value = serde_json::from_str(&guest_descriptor).unwrap();
        let template = wasm_template_with_duplicated_descriptor();
        let contribution = &template.module_contributions[0];
        let implementation = &contribution.implementations[0];

        assert_eq!(
            (
                contribution.provides[0].capability_id.as_str(),
                contribution.provides[0].descriptor_version.as_str(),
                contribution.provides[0].request_operations.as_slice(),
                implementation.artifact.as_deref(),
                implementation.execution_class.as_str(),
                implementation.targets.as_slice(),
                implementation.trust,
            ),
            (
                guest["capabilities"][0]["capability_id"].as_str().unwrap(),
                guest["capabilities"][0]["descriptor_version"]
                    .as_str()
                    .unwrap(),
                &["echo".to_owned()][..],
                Some("guest"),
                "lenso.wasm-component@1",
                &["wasm32-unknown-unknown".to_owned()][..],
                TrustLevel::Constrained,
            )
        );
    }

    #[test]
    fn bundle_build_requires_a_publisher_template_before_reading_artifacts() {
        let source = tempfile::tempdir().unwrap();
        let error = build_bundle(&BundleBuild {
            template: source.path().join("missing-template.json"),
            output: source.path().join("dist/plugin"),
            artifact_sources: Vec::new(),
        })
        .unwrap_err();

        assert!(error.to_string().contains("Plugin Manifest template"));
    }

    #[test]
    fn source_metadata_rejects_publisher_owned_runtime_fields() {
        let error = toml::from_str::<CargoManifest>(
            r#"
                [package]
                version = "1.0.0"

                [package.metadata.lenso]
                plugin-id = "example.echo"
                module-contributions = []
            "#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("module-contributions"));
    }

    #[test]
    fn descriptor_extraction_rejects_missing_evidence() {
        let error = extract_plugin_descriptor(&wasm_with_descriptors(&[])).unwrap_err();

        assert!(error.to_string().contains("does not contain"));
    }

    #[test]
    fn descriptor_extraction_rejects_duplicate_evidence() {
        let descriptor = br#"{"profile":"one"}"#;
        let error = extract_plugin_descriptor(&wasm_with_descriptors(&[
            descriptor.as_slice(),
            descriptor.as_slice(),
        ]))
        .unwrap_err();

        assert!(error.to_string().contains("duplicate"));
    }

    #[test]
    fn descriptor_extraction_rejects_oversized_evidence() {
        let descriptor = vec![b' '; MAX_PLUGIN_DESCRIPTOR_BYTES + 1];
        let error = extract_plugin_descriptor(&wasm_with_descriptors(&[&descriptor])).unwrap_err();

        assert!(error.to_string().contains("size limit"));
    }

    #[test]
    fn descriptor_extraction_rejects_malformed_or_noncanonical_json() {
        let malformed = extract_plugin_descriptor(&wasm_with_descriptors(&[b"{"])).unwrap_err();
        let noncanonical =
            extract_plugin_descriptor(&wasm_with_descriptors(&[br#"{ "profile": "one" }"#]))
                .unwrap_err();

        assert!(
            malformed.to_string().contains("invalid Plugin Manifest")
                && noncanonical.to_string().contains("not canonical")
        );
    }

    #[test]
    fn descriptor_changes_are_closed_by_the_final_artifact_digest() {
        let first = wasm_with_descriptors(&[br#"{"profile":"one"}"#]);
        let second = wasm_with_descriptors(&[br#"{"profile":"two"}"#]);

        assert_ne!(sha256_digest(&first), sha256_digest(&second));
    }

    #[test]
    fn v1_creation_and_verification_remain_compatible_during_migration() {
        let source = tempfile::tempdir().unwrap();
        let template_path = source.path().join("lenso-plugin.template.json");
        fs::write(&template_path, serde_json::to_vec(&template()).unwrap()).unwrap();
        fs::write(
            source.path().join("source.mjs"),
            b"export const ready = true;\n",
        )
        .unwrap();
        let output = source.path().join("dist/plugin");
        let built = build_bundle(&BundleBuild {
            template: template_path,
            output: output.clone(),
            artifact_sources: vec![ArtifactSource {
                artifact_id: "script".to_owned(),
                path: source.path().join("source.mjs"),
            }],
        })
        .unwrap();
        assert_eq!(built, verify_bundle_directory(&output).unwrap());
        assert_ne!(built.artifact_digests, [ZERO_DIGEST]);
    }

    #[test]
    fn verification_rejects_tampering_and_undeclared_files() {
        let mut manifest = template();
        let bytes = b"ready".to_vec();
        manifest.artifacts[0].digest = sha256_digest(&bytes);
        manifest.artifacts[0].size = u64::try_from(bytes.len()).unwrap();
        let document = ManifestDocument::from_value(manifest).unwrap();
        let mut files = BTreeMap::from([("plugin.mjs".to_owned(), bytes)]);
        verify_bundle_files(&document, &files).unwrap();
        files.insert("extra".to_owned(), Vec::new());
        assert!(verify_bundle_files(&document, &files).is_err());
    }

    #[test]
    fn parsing_rejects_duplicate_fields_and_path_escape() {
        assert!(ManifestDocument::parse(br#"{"schema_version":1,"schema_version":1}"#).is_err());
        let mut manifest = template();
        manifest.artifacts[0].path = "../plugin.mjs".to_owned();
        assert!(ManifestDocument::from_value(manifest).is_err());
    }
}
