use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
};

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
        validate_manifest(manifest.value())?;

        let declared_paths: BTreeSet<_> = manifest
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
            .collect();
        if declared_paths.len()
            != manifest.value().artifacts.len() + manifest.value().product_metadata.len()
        {
            return rejected("Plugin Manifest declares a duplicate Bundle path");
        }
        if bundle
            .files
            .keys()
            .any(|path| !declared_paths.contains(path.as_str()))
        {
            return rejected("Bundle contains an undeclared file");
        }

        let mut artifact_digests = Vec::with_capacity(manifest.value().artifacts.len());
        for declaration in &manifest.value().artifacts {
            validate_relative_path(&declaration.path)?;
            let bytes = bundle.files.get(&declaration.path).ok_or_else(|| {
                ControlPlaneError::AdmissionRejected {
                    detail: format!("Bundle is missing Artifact `{}`", declaration.id),
                }
            })?;
            if declaration.size > self.max_artifact_bytes
                || u64::try_from(bytes.len()).unwrap_or(u64::MAX) != declaration.size
            {
                return rejected("Artifact exceeds its declared or Store byte limit");
            }
            if sha256_digest(bytes) != declaration.digest {
                return Err(ControlPlaneError::DigestMismatch {
                    subject: format!("Artifact `{}`", declaration.id),
                });
            }
            artifact_digests.push(declaration.digest.clone());
        }
        artifact_digests.sort();
        artifact_digests.dedup();

        let mut product_metadata_digests = Vec::new();
        for metadata in &manifest.value().product_metadata {
            validate_relative_path(&metadata.path)?;
            let bytes = bundle.files.get(&metadata.path).ok_or_else(|| {
                ControlPlaneError::AdmissionRejected {
                    detail: format!("Bundle is missing Product Metadata `{}`", metadata.id),
                }
            })?;
            if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > self.max_artifact_bytes
                || sha256_digest(bytes) != metadata.digest
            {
                return rejected("Product Metadata exceeds limits or its declared digest");
            }
            product_metadata_digests.push(metadata.digest.clone());
        }
        product_metadata_digests.sort();
        product_metadata_digests.dedup();

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

#[allow(clippy::too_many_lines)]
fn validate_manifest(manifest: &PluginManifest) -> Result<(), ControlPlaneError> {
    if manifest.schema_version != 1 {
        return rejected("unsupported Plugin Manifest schema version");
    }
    if manifest.plugin_id.is_empty() || semver::Version::parse(&manifest.release_version).is_err() {
        return rejected("Plugin identity or Release version is invalid");
    }
    let artifact_ids: Vec<_> = manifest
        .artifacts
        .iter()
        .map(|artifact| artifact.id.as_str())
        .collect();
    ensure_strictly_sorted_unique(&artifact_ids, "Artifact ID")?;
    let contribution_ids: Vec<_> = manifest
        .module_contributions
        .iter()
        .map(|contribution| contribution.id.as_str())
        .collect();
    ensure_strictly_sorted_unique(&contribution_ids, "Module contribution ID")?;
    let data_ids = manifest
        .data_contributions
        .iter()
        .map(|contribution| contribution.id.as_str())
        .collect::<Vec<_>>();
    ensure_strictly_sorted_unique(&data_ids, "Data contribution ID")?;
    let permission_ids = manifest
        .permission_requests
        .iter()
        .map(|request| request.id.as_str())
        .collect::<Vec<_>>();
    ensure_strictly_sorted_unique(&permission_ids, "permission request ID")?;
    let feature_ids = manifest
        .features
        .iter()
        .map(|feature| feature.id.as_str())
        .collect::<Vec<_>>();
    ensure_strictly_sorted_unique(&feature_ids, "Feature ID")?;
    let metadata_ids = manifest
        .product_metadata
        .iter()
        .map(|metadata| metadata.id.as_str())
        .collect::<Vec<_>>();
    ensure_strictly_sorted_unique(&metadata_ids, "Product Metadata ID")?;
    let artifact_id_set = artifact_ids.iter().copied().collect::<BTreeSet<_>>();
    let contribution_id_set = contribution_ids.iter().copied().collect::<BTreeSet<_>>();
    let data_id_set = data_ids.iter().copied().collect::<BTreeSet<_>>();
    let permission_id_set = permission_ids.iter().copied().collect::<BTreeSet<_>>();
    let metadata_id_set = metadata_ids.iter().copied().collect::<BTreeSet<_>>();
    for artifact in &manifest.artifacts {
        digest_component(&artifact.digest)?;
        if artifact.id.is_empty() || artifact.media_type.is_empty() || artifact.targets.is_empty() {
            return rejected("Artifact identity, media type, and targets must be explicit");
        }
        let targets = artifact
            .targets
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        ensure_strictly_sorted_unique(&targets, "Artifact target")?;
    }
    for contribution in &manifest.module_contributions {
        digest_component(&contribution.configuration_schema_digest)?;
        if contribution.id.is_empty()
            || contribution.package_id.is_empty()
            || contribution.implementations.is_empty()
        {
            return rejected("Module contribution identity and implementation set are required");
        }
        let provided = contribution
            .provides
            .iter()
            .map(|capability| capability.capability_id.as_str())
            .collect::<Vec<_>>();
        ensure_strictly_sorted_unique(&provided, "provided Capability")?;
        for capability in &contribution.provides {
            digest_component(&capability.descriptor_digest)?;
            semver::Version::parse(&capability.descriptor_version).map_err(|_| {
                ControlPlaneError::AdmissionRejected {
                    detail: "Capability Descriptor version is not SemVer".to_owned(),
                }
            })?;
            let operations = capability
                .request_operations
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            ensure_strictly_sorted_unique(&operations, "Request Operation")?;
        }
        let required = contribution
            .requires
            .iter()
            .map(|capability| capability.capability_id.as_str())
            .collect::<Vec<_>>();
        ensure_strictly_sorted_unique(&required, "required Capability")?;
        for capability in &contribution.requires {
            semver::Version::parse(&capability.descriptor_version).map_err(|_| {
                ControlPlaneError::AdmissionRejected {
                    detail: "required Capability Descriptor version is not SemVer".to_owned(),
                }
            })?;
        }
        let requests = contribution
            .permission_request_ids
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        ensure_strictly_sorted_unique(&requests, "Module permission request")?;
        if requests
            .iter()
            .any(|request| !permission_id_set.contains(request))
        {
            return rejected("Module contribution references an unknown permission request");
        }
        if let Some(state) = &contribution.state {
            if state.state_schema_id.is_empty() {
                return rejected("state Schema identity is empty");
            }
            digest_component(&state.state_schema_digest)?;
        }
        let variants: Vec<_> = contribution
            .implementations
            .iter()
            .map(|variant| variant.id.as_str())
            .collect();
        ensure_strictly_sorted_unique(&variants, "implementation variant ID")?;
        for variant in &contribution.implementations {
            if variant.artifact.is_some() == variant.built_in_factory.is_some() {
                return rejected("implementation variant must select exactly one execution input");
            }
            if variant.built_in_factory.is_some()
                && variant.execution_class != "lenso.native-rust@1"
            {
                return rejected("only native Rust may select a built-in factory");
            }
            if variant
                .artifact
                .as_deref()
                .is_some_and(|artifact| !artifact_id_set.contains(artifact))
            {
                return rejected("implementation variant references an unknown Artifact");
            }
            if variant.entrypoint.is_empty()
                || variant.execution_class.is_empty()
                || variant.targets.is_empty()
                || variant.profiles.is_empty()
            {
                return rejected(
                    "implementation entrypoint, class, targets, and Profiles are required",
                );
            }
            let targets = variant
                .targets
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            let profiles = variant
                .profiles
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            ensure_strictly_sorted_unique(&targets, "implementation target")?;
            ensure_strictly_sorted_unique(&profiles, "implementation Profile")?;
        }
    }
    for contribution in &manifest.data_contributions {
        if !artifact_id_set.contains(contribution.artifact.as_str())
            || contribution.media_type.is_empty()
            || contribution.content_schema_id.is_empty()
        {
            return rejected("Data contribution references unknown or incomplete authority");
        }
        digest_component(&contribution.content_schema_digest)?;
        digest_component(&contribution.product_metadata_digest)?;
    }
    for request in &manifest.permission_requests {
        if request.id.is_empty()
            || request.resource_kind.is_empty()
            || request.explanation_key.is_empty()
        {
            return rejected("permission request identity is incomplete");
        }
    }
    for metadata in &manifest.product_metadata {
        if metadata.id.is_empty() || metadata.namespace.is_empty() || metadata.schema_id.is_empty()
        {
            return rejected("Product Metadata identity is incomplete");
        }
        validate_relative_path(&metadata.path)?;
        digest_component(&metadata.digest)?;
    }
    for feature in &manifest.features {
        validate_feature_refs(
            &feature.module_contribution_ids,
            &contribution_id_set,
            "Module contribution",
        )?;
        validate_feature_refs(
            &feature.data_contribution_ids,
            &data_id_set,
            "Data contribution",
        )?;
        validate_feature_refs(&feature.artifact_ids, &artifact_id_set, "Artifact")?;
        validate_feature_refs(
            &feature.permission_request_ids,
            &permission_id_set,
            "permission request",
        )?;
        validate_feature_refs(
            &feature.product_metadata_ids,
            &metadata_id_set,
            "Product Metadata",
        )?;
    }
    for template in &manifest.binding_templates {
        if !contribution_id_set.contains(template.consumer_contribution_id.as_str())
            || !contribution_id_set.contains(template.provider_contribution_id.as_str())
            || template.capability_id.is_empty()
        {
            return rejected("binding template references unknown contribution authority");
        }
        let consumer = manifest
            .module_contributions
            .iter()
            .find(|contribution| contribution.id == template.consumer_contribution_id)
            .expect("contribution identity was validated");
        let provider = manifest
            .module_contributions
            .iter()
            .find(|contribution| contribution.id == template.provider_contribution_id)
            .expect("contribution identity was validated");
        if !consumer
            .requires
            .iter()
            .any(|requirement| requirement.capability_id == template.capability_id)
            || !provider
                .provides
                .iter()
                .any(|capability| capability.capability_id == template.capability_id)
        {
            return rejected("binding template Capability is not required and provided exactly");
        }
    }
    Ok(())
}

fn validate_feature_refs<'a>(
    values: &'a [String],
    known: &BTreeSet<&'a str>,
    kind: &str,
) -> Result<(), ControlPlaneError> {
    let refs = values.iter().map(String::as_str).collect::<Vec<_>>();
    ensure_strictly_sorted_unique(&refs, kind)?;
    if refs.iter().any(|value| !known.contains(value)) {
        return rejected(format!("Feature references an unknown {kind}"));
    }
    Ok(())
}

fn validate_relative_path(path: &str) -> Result<(), ControlPlaneError> {
    if path.is_empty() || path.contains('\\') {
        return rejected("Bundle path is empty or platform-ambiguous");
    }
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return rejected("Bundle path must contain only normalized relative segments");
    }
    Ok(())
}

fn ensure_strictly_sorted_unique<T: Ord + std::fmt::Display>(
    values: &[T],
    kind: &str,
) -> Result<(), ControlPlaneError> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return rejected(format!("{kind} entries must be sorted and unique"));
    }
    Ok(())
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
