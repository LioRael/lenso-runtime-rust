//! Immutable Plugin Release manifests, source materialization, and Bundle verification.

mod model;

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::{Component, Path, PathBuf},
};

use lenso_app_plan::{
    CapabilityEndpointPlan, CapabilityOperationKind, CapabilityRequirementPlan, ExecutionClassId,
    authoring::PluginDescriptor,
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

/// Source-only input for one generated V2 Plugin Bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourcePluginBuild {
    pub package_manifest: PathBuf,
    pub wasm_module: PathBuf,
    pub output: PathBuf,
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
    root_slot: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GuestRuntimeDescriptor {
    abi: String,
    capabilities: Vec<GuestCapability>,
    #[serde(default)]
    required_capabilities: Vec<GuestRequirement>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GuestCapability {
    capability_id: String,
    descriptor_version: String,
    request_operations: Vec<String>,
    #[serde(default)]
    stream_operations: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GuestRequirement {
    capability_id: String,
    descriptor_version: String,
    cardinality: String,
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
    let runtime_descriptor = extract_plugin_descriptor(&component)?;
    let artifact = PluginArtifactV2 {
        path: "plugin.wasm".to_owned(),
        digest: sha256_digest(&component),
        size: u64::try_from(component.len())
            .map_err(|_| BundleError::InvalidBundle("Artifact size exceeds u64".to_owned()))?,
        media_type: "application/wasm".to_owned(),
        target: "wasm32-unknown-unknown".to_owned(),
    };
    let descriptor = portable_plugin_descriptor(
        &package.package.metadata.lenso.plugin_id,
        &package.package.version,
        &package.package.metadata.lenso.root_slot,
        &artifact.digest,
        &runtime_descriptor,
    )?;
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

/// Verifies an already materialized directory as an exact immutable Bundle closure.
pub fn verify_bundle_directory(root: &Path) -> Result<VerifiedBundle, BundleError> {
    let manifest_path = root.join(MANIFEST_FILE);
    let manifest_bytes = read_regular_file(&manifest_path, "Plugin Manifest")?;
    let mut files = BTreeMap::new();
    collect_bundle_files(root, root, &mut files)?;
    files.remove(MANIFEST_FILE);
    verify_source_bundle_files(&SourceManifestDocument::parse(&manifest_bytes)?, &files)
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
    let runtime_descriptor = extract_plugin_descriptor(bytes)?;
    let descriptor = portable_plugin_descriptor(
        &manifest.value.plugin_id,
        &manifest.value.release_version,
        manifest
            .value
            .entry
            .descriptor
            .get("root_slot")
            .and_then(Value::as_str)
            .ok_or_else(|| BundleError::InvalidManifest("root_slot is required".to_owned()))?,
        &artifact.digest,
        &runtime_descriptor,
    )?;
    let packaged = serde_json::to_vec(&manifest.value.entry.descriptor)
        .map_err(|error| BundleError::InvalidManifest(error.to_string()))?;
    let derived = serde_json::to_vec(&descriptor)
        .map_err(|error| BundleError::InvalidManifest(error.to_string()))?;
    if derived != packaged {
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

fn portable_plugin_descriptor(
    plugin_id: &str,
    release_version: &str,
    root_slot: &str,
    artifact_digest: &str,
    encoded: &[u8],
) -> Result<Value, BundleError> {
    let runtime = strict_json::<GuestRuntimeDescriptor>(encoded)?;
    if ![
        "lenso.json-request@1",
        "lenso.json-interactions@1",
        "lenso.json-host-imports@1",
    ]
    .contains(&runtime.abi.as_str())
    {
        return invalid_manifest("unsupported guest Plugin ABI");
    }
    let mut descriptor = PluginDescriptor::new(plugin_id, release_version, root_slot)
        .with_runtime_package(plugin_id, artifact_digest)
        .with_execution_class(ExecutionClassId::new("lenso.wasm-component@1"));
    for capability in runtime.capabilities {
        let mut endpoint = CapabilityEndpointPlan::new(
            capability.capability_id,
            capability.descriptor_version,
            capability
                .request_operations
                .iter()
                .chain(&capability.stream_operations)
                .cloned(),
        );
        for operation in capability.stream_operations {
            endpoint = endpoint.with_operation_kind(operation, CapabilityOperationKind::Stream);
        }
        descriptor = descriptor.with_capability(endpoint);
    }
    for requirement in runtime.required_capabilities {
        if requirement.cardinality != "one" {
            return invalid_manifest("unsupported guest Capability cardinality");
        }
        descriptor = descriptor.with_requirement(CapabilityRequirementPlan::one(
            requirement.capability_id,
            requirement.descriptor_version,
        ));
    }
    serde_json::to_value(descriptor)
        .map_err(|error| BundleError::InvalidManifest(error.to_string()))
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
    fn source_metadata_rejects_old_multi_entry_fields() {
        let error = toml::from_str::<CargoManifest>(
            r#"
                [package]
                version = "1.0.0"

                [package.metadata.lenso]
                plugin-id = "example.echo"
                root-slot = "tools"
                module-contributions = []
            "#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("module-contributions"));
    }

    #[test]
    fn descriptor_extraction_requires_one_canonical_descriptor() {
        assert!(extract_plugin_descriptor(&wasm_with_descriptors(&[])).is_err());
        let descriptor = br#"{"profile":"one"}"#;
        assert!(
            extract_plugin_descriptor(&wasm_with_descriptors(&[
                descriptor.as_slice(),
                descriptor.as_slice(),
            ]))
            .is_err()
        );
        assert!(extract_plugin_descriptor(&wasm_with_descriptors(&[b"{"])).is_err());
        assert!(
            extract_plugin_descriptor(&wasm_with_descriptors(&[br#"{ "profile": "one" }"#]))
                .is_err()
        );
    }

    #[test]
    fn descriptor_extraction_rejects_oversized_evidence() {
        let descriptor = vec![b' '; MAX_PLUGIN_DESCRIPTOR_BYTES + 1];
        assert!(extract_plugin_descriptor(&wasm_with_descriptors(&[&descriptor])).is_err());
    }

    #[test]
    fn strict_v2_manifest_rejects_duplicate_fields_and_path_escape() {
        assert!(
            SourceManifestDocument::parse(br#"{"schema_version":2,"schema_version":2}"#).is_err()
        );
        let manifest = PluginManifestV2 {
            schema_version: 2,
            plugin_id: "example.echo".to_owned(),
            release_version: "1.0.0".to_owned(),
            artifact: PluginArtifactV2 {
                path: "../plugin.wasm".to_owned(),
                digest: sha256_digest(b"plugin"),
                size: 6,
                media_type: "application/wasm".to_owned(),
                target: "wasm32-unknown-unknown".to_owned(),
            },
            entry: PluginEntryV2 {
                descriptor: serde_json::json!({"plugin_id":"example.echo"}),
            },
        };
        assert!(SourceManifestDocument::from_value(manifest).is_err());
    }
}
