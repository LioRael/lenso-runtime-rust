use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use lenso_plugin_bundle::{BundleError, ManifestDocument, verify_bundle_files};
use serde::{Deserialize, Serialize};

use crate::{
    ArtifactDeclaration, CanonicalDocument, ControlPlaneError, PluginManifest, sha256_digest,
};

/// Detached Bundle bytes presented to generic admission.
#[derive(Clone, Debug)]
pub struct PluginBundle {
    manifest: Vec<u8>,
    files: BTreeMap<String, Vec<u8>>,
    provenance: String,
}

impl PluginBundle {
    /// Creates a Bundle from exact Manifest bytes and normalized relative file entries.
    pub fn new(
        manifest: impl Into<Vec<u8>>,
        files: BTreeMap<String, Vec<u8>>,
        provenance: impl Into<String>,
    ) -> Self {
        Self {
            manifest: manifest.into(),
            files,
            provenance: provenance.into(),
        }
    }
}

/// Operator-owned admission policy. A publisher signature never self-authorizes.
pub trait AdmissionPolicy: std::fmt::Debug {
    /// Returns bounded decision evidence or rejects this exact immutable Release.
    fn admit(
        &self,
        manifest: &PluginManifest,
        manifest_digest: &str,
        artifact_digests: &[String],
        product_metadata_digests: &[String],
        provenance: &str,
    ) -> Result<String, ControlPlaneError>;

    /// Stable identity recorded in Admission Receipts.
    fn identity(&self) -> &'static str;
}

/// Immutable operator admission decision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionReceipt {
    pub schema_version: u32,
    pub policy_identity: String,
    pub plugin_id: String,
    pub release_version: String,
    pub manifest_digest: String,
    pub artifact_digests: Vec<String>,
    pub product_metadata_digests: Vec<String>,
    pub provenance: String,
    pub decision_evidence: String,
}

/// One admitted Store Artifact resolved by content identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedArtifact {
    pub artifact_id: String,
    pub digest: String,
    pub size: u64,
    pub media_type: String,
    pub path: PathBuf,
}

/// Read-only source for exact Plugin Artifact bytes during Generation resolution.
pub trait ArtifactSource: std::fmt::Debug {
    /// Resolves and verifies one Artifact declared by the selected Plugin Release.
    fn artifact(
        &self,
        declaration: &ArtifactDeclaration,
    ) -> Result<AdmittedArtifact, ControlPlaneError>;
}

/// Fail-closed source for Host-built Plugin selections that declare no Artifacts.
#[derive(Debug, Default)]
pub struct NoArtifactSource;

impl ArtifactSource for NoArtifactSource {
    fn artifact(
        &self,
        declaration: &ArtifactDeclaration,
    ) -> Result<AdmittedArtifact, ControlPlaneError> {
        rejected(format!(
            "no Artifact source is configured for selected Artifact `{}`",
            declaration.id
        ))
    }
}

/// Content-addressed Plugin Store with no mutable "latest" authority.
#[derive(Debug)]
pub struct PluginStore {
    root: PathBuf,
    max_manifest_bytes: usize,
    max_artifact_bytes: u64,
}

impl PluginStore {
    /// Opens or creates a Store root with bounded admission defaults.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, ControlPlaneError> {
        let root = root.into();
        fs::create_dir_all(root.join("objects")).map_err(store_error)?;
        fs::create_dir_all(root.join("manifests")).map_err(store_error)?;
        fs::create_dir_all(root.join("receipts")).map_err(store_error)?;
        Ok(Self {
            root,
            max_manifest_bytes: 1024 * 1024,
            max_artifact_bytes: 256 * 1024 * 1024,
        })
    }

    /// Applies stricter Store-wide byte limits.
    #[must_use]
    pub const fn with_limits(mut self, max_manifest_bytes: usize, max_artifact_bytes: u64) -> Self {
        self.max_manifest_bytes = max_manifest_bytes;
        self.max_artifact_bytes = max_artifact_bytes;
        self
    }

    /// Verifies and atomically admits one immutable Plugin Release.
    #[allow(clippy::too_many_lines)]
    pub fn admit(
        &self,
        bundle: &PluginBundle,
        policy: &dyn AdmissionPolicy,
    ) -> Result<CanonicalDocument<AdmissionReceipt>, ControlPlaneError> {
        if bundle.manifest.len() > self.max_manifest_bytes {
            return rejected("Plugin Manifest exceeds Store byte limit");
        }
        let manifest =
            CanonicalDocument::<PluginManifest>::parse("lenso-plugin.json", &bundle.manifest)?;
        if manifest
            .value()
            .artifacts
            .iter()
            .any(|artifact| artifact.size > self.max_artifact_bytes)
            || manifest.value().product_metadata.iter().any(|metadata| {
                bundle.files.get(&metadata.path).is_some_and(|bytes| {
                    u64::try_from(bytes.len()).unwrap_or(u64::MAX) > self.max_artifact_bytes
                })
            })
        {
            return rejected("Bundle file exceeds Store byte limit");
        }
        let bundle_manifest =
            ManifestDocument::from_value(manifest.value().clone()).map_err(bundle_error)?;
        if bundle_manifest.digest() != manifest.digest() {
            return Err(ControlPlaneError::DigestMismatch {
                subject: "Plugin Manifest".to_owned(),
            });
        }
        let verified =
            verify_bundle_files(&bundle_manifest, &bundle.files).map_err(bundle_error)?;
        let artifact_digests = verified.artifact_digests;
        let product_metadata_digests = verified.product_metadata_digests;

        let evidence = policy.admit(
            manifest.value(),
            manifest.digest(),
            &artifact_digests,
            &product_metadata_digests,
            &bundle.provenance,
        )?;
        let receipt = CanonicalDocument::from_value(
            "Admission Receipt",
            AdmissionReceipt {
                schema_version: 1,
                policy_identity: policy.identity().to_owned(),
                plugin_id: manifest.value().plugin_id.clone(),
                release_version: manifest.value().release_version.clone(),
                manifest_digest: manifest.digest().to_owned(),
                artifact_digests,
                product_metadata_digests,
                provenance: bundle.provenance.clone(),
                decision_evidence: evidence,
            },
        )?;

        for declaration in &manifest.value().artifacts {
            let bytes = bundle
                .files
                .get(&declaration.path)
                .expect("Bundle closure was validated");
            self.commit_object(&declaration.digest, bytes)?;
        }
        for metadata in &manifest.value().product_metadata {
            let bytes = bundle
                .files
                .get(&metadata.path)
                .expect("Bundle closure was validated");
            self.commit_object(&metadata.digest, bytes)?;
        }
        self.commit_named("manifests", manifest.digest(), manifest.bytes())?;
        self.commit_named("receipts", receipt.digest(), receipt.bytes())?;
        Ok(receipt)
    }

    /// Resolves an admitted Artifact and verifies Store bytes again before staging.
    pub fn artifact(
        &self,
        declaration: &ArtifactDeclaration,
    ) -> Result<AdmittedArtifact, ControlPlaneError> {
        let path = self.object_path(&declaration.digest)?;
        let metadata = fs::symlink_metadata(&path).map_err(store_error)?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return rejected("stored Artifact is not a regular immutable file");
        }
        let bytes = fs::read(&path).map_err(store_error)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != declaration.size
            || sha256_digest(&bytes) != declaration.digest
        {
            return Err(ControlPlaneError::DigestMismatch {
                subject: format!("stored Artifact `{}`", declaration.id),
            });
        }
        Ok(AdmittedArtifact {
            artifact_id: declaration.id.clone(),
            digest: declaration.digest.clone(),
            size: declaration.size,
            media_type: declaration.media_type.clone(),
            path,
        })
    }

    /// Loads and digest-verifies one immutable admission receipt.
    pub fn admission_receipt(
        &self,
        digest: &str,
    ) -> Result<CanonicalDocument<AdmissionReceipt>, ControlPlaneError> {
        let name = digest_component(digest)?;
        let path = self.root.join("receipts").join(name);
        let metadata = fs::symlink_metadata(&path).map_err(store_error)?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return rejected("Admission Receipt is not a regular immutable file");
        }
        let bytes = fs::read(path).map_err(store_error)?;
        let receipt = CanonicalDocument::parse("Admission Receipt", &bytes)?;
        if receipt.digest() != digest {
            return Err(ControlPlaneError::DigestMismatch {
                subject: "Admission Receipt".to_owned(),
            });
        }
        Ok(receipt)
    }

    fn commit_object(&self, digest: &str, bytes: &[u8]) -> Result<(), ControlPlaneError> {
        let destination = self.object_path(digest)?;
        commit_immutable(&destination, bytes)
    }

    fn commit_named(
        &self,
        namespace: &str,
        digest: &str,
        bytes: &[u8],
    ) -> Result<(), ControlPlaneError> {
        let name = digest_component(digest)?;
        commit_immutable(&self.root.join(namespace).join(name), bytes)
    }

    fn object_path(&self, digest: &str) -> Result<PathBuf, ControlPlaneError> {
        Ok(self.root.join("objects").join(digest_component(digest)?))
    }
}

impl ArtifactSource for PluginStore {
    fn artifact(
        &self,
        declaration: &ArtifactDeclaration,
    ) -> Result<AdmittedArtifact, ControlPlaneError> {
        Self::artifact(self, declaration)
    }
}

fn digest_component(digest: &str) -> Result<&str, ControlPlaneError> {
    let Some(value) = digest.strip_prefix("sha256:") else {
        return rejected("digest does not use sha256 prefix");
    };
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return rejected("digest is not 64 lowercase hexadecimal characters");
    }
    Ok(value)
}

fn commit_immutable(destination: &Path, bytes: &[u8]) -> Result<(), ControlPlaneError> {
    if destination.exists() {
        let metadata = fs::symlink_metadata(destination).map_err(store_error)?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(ControlPlaneError::StoreFailure {
                detail: format!(
                    "Store object `{}` is not a regular file",
                    destination.display()
                ),
            });
        }
        let existing = fs::read(destination).map_err(store_error)?;
        if existing == bytes {
            return Ok(());
        }
        return Err(ControlPlaneError::StoreFailure {
            detail: format!("immutable Store object `{}` changed", destination.display()),
        });
    }
    let temporary = (0_u16..=u16::MAX)
        .map(|attempt| destination.with_extension(format!("{}-{attempt}.tmp", std::process::id())))
        .find(|candidate| !candidate.exists())
        .ok_or_else(|| ControlPlaneError::StoreFailure {
            detail: "cannot allocate an immutable Store temporary file".to_owned(),
        })?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(store_error)?;
    file.write_all(bytes).map_err(store_error)?;
    file.sync_all().map_err(store_error)?;
    match fs::hard_link(&temporary, destination) {
        Ok(()) => {
            let mut permissions = fs::metadata(destination)
                .map_err(store_error)?
                .permissions();
            permissions.set_readonly(true);
            fs::set_permissions(destination, permissions).map_err(store_error)?;
            fs::remove_file(temporary).map_err(store_error)?;
            sync_parent(destination)
        }
        Err(_error) if destination.exists() => {
            let _ = fs::remove_file(&temporary);
            let existing = fs::read(destination).map_err(store_error)?;
            if existing == bytes {
                Ok(())
            } else {
                Err(ControlPlaneError::StoreFailure {
                    detail: format!(
                        "immutable Store object `{}` raced with different bytes",
                        destination.display()
                    ),
                })
            }
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(store_error(error))
        }
    }
}

fn sync_parent(path: &Path) -> Result<(), ControlPlaneError> {
    let parent = path
        .parent()
        .ok_or_else(|| ControlPlaneError::StoreFailure {
            detail: "Store object has no parent directory".to_owned(),
        })?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(store_error)
}

fn rejected<T>(detail: impl Into<String>) -> Result<T, ControlPlaneError> {
    Err(ControlPlaneError::AdmissionRejected {
        detail: detail.into(),
    })
}

fn store_error(error: impl std::fmt::Display) -> ControlPlaneError {
    ControlPlaneError::StoreFailure {
        detail: error.to_string(),
    }
}

fn bundle_error(error: BundleError) -> ControlPlaneError {
    match error {
        BundleError::DigestMismatch(subject) => ControlPlaneError::DigestMismatch { subject },
        error => ControlPlaneError::AdmissionRejected {
            detail: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use lenso_app_plan::CapabilityOperationKind;

    use super::*;
    use crate::{
        ArtifactKind, CapabilityDeclaration, ImplementationVariant, ModuleContribution,
        SupportChannel, TrustLevel,
    };

    #[test]
    fn absent_artifact_source_fails_closed() {
        let declaration = ArtifactDeclaration {
            id: "unavailable".to_owned(),
            kind: ArtifactKind::QuickJsModule,
            digest: sha256_digest(b"artifact"),
            size: 8,
            media_type: "text/javascript".to_owned(),
            path: "plugin.mjs".to_owned(),
            targets: vec!["test-target".to_owned()],
        };

        let error = NoArtifactSource.artifact(&declaration).unwrap_err();

        assert!(matches!(
            &error,
            ControlPlaneError::AdmissionRejected { .. }
        ));
        assert!(error.to_string().contains("no Artifact source"));
    }

    #[test]
    fn interaction_kind_must_name_a_declared_operation() {
        let manifest = PluginManifest {
            schema_version: 1,
            plugin_id: "example.invalid-kind".to_owned(),
            release_version: "1.0.0".to_owned(),
            artifacts: Vec::new(),
            module_contributions: vec![ModuleContribution {
                id: "provider".to_owned(),
                package_id: "example.provider".to_owned(),
                configuration_schema_digest: sha256_digest(b"configuration"),
                provides: vec![CapabilityDeclaration {
                    capability_id: "example.echo@1".to_owned(),
                    descriptor_version: "1.0.0".to_owned(),
                    descriptor_digest: sha256_digest(b"descriptor"),
                    request_operations: vec!["echo".to_owned()],
                    operation_kinds: BTreeMap::from([(
                        "missing".to_owned(),
                        CapabilityOperationKind::Stream,
                    )]),
                }],
                requires: Vec::new(),
                implementations: vec![ImplementationVariant {
                    id: "native".to_owned(),
                    artifact: None,
                    built_in_factory: Some("example.provider@1.0.0".to_owned()),
                    entrypoint: "default".to_owned(),
                    execution_class: "lenso.native-rust@1".to_owned(),
                    targets: vec!["test-target".to_owned()],
                    profiles: vec!["test-v1".to_owned()],
                    support_channel: SupportChannel::Stable,
                    trust: TrustLevel::Trusted,
                }],
                permission_request_ids: Vec::new(),
                state: None,
            }],
            data_contributions: Vec::new(),
            permission_requests: Vec::new(),
            features: Vec::new(),
            binding_templates: Vec::new(),
            product_metadata: Vec::new(),
        };

        let error = lenso_plugin_bundle::validate_manifest(&manifest).unwrap_err();
        assert!(error.to_string().contains("unknown Operation"));
    }
}
