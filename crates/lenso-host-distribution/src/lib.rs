//! Fail-closed assembly of one prepared Host distribution and an external Plugin Root.
//!
//! The CLI authors the immutable distribution. This crate independently verifies those
//! bytes, invokes the bundled same-cohort resolver, and closes its Plan over exact
//! Instance-addressed Artifacts. Execution Adapters remain a product Host concern.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use lenso_app_plan::ResolvedAppPlan;
use lenso_plugin_control_plane::{
    AdapterProfile, CanonicalDocument, HostBuildManifest, HostExecutionPolicy, PlanArtifact,
    PlanGenerationInput, ResolvedGeneration, resolve_plan_generation, strict_json,
};
use lenso_runtime_codec::{ArtifactHandle, InstanceResourceCatalog};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const LOCK_LIMIT: u64 = 8 * 1024 * 1024;
const INVENTORY_LIMIT: u64 = 8 * 1024 * 1024;
const RESOLVER_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
const RESOLVER_ERROR_LIMIT: usize = 64 * 1024;
const RESOLVER_TIMEOUT: Duration = Duration::from_secs(30);

/// A verified immutable distribution and its identity.
#[derive(Clone, Debug)]
pub struct VerifiedDistribution {
    root: PathBuf,
    identity: String,
    lock: DistributionLock,
    files: BTreeMap<String, DistributionFile>,
}

impl VerifiedDistribution {
    /// Independently verifies the lock, current platform, and every declared file.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, DistributionError> {
        let root = fs::canonicalize(root.as_ref())
            .map_err(|error| io_error("locate distribution", error))?;
        if !fs::metadata(&root)
            .map_err(|error| io_error("inspect distribution", error))?
            .is_dir()
        {
            return Err(invalid("distribution root must be a directory"));
        }
        let lock_path = root.join(".lenso/distribution.lock.json");
        let lock_bytes = read_regular_bounded(&lock_path, LOCK_LIMIT, "distribution lock")?;
        let lock: DistributionLock = strict_json("distribution.lock.json", &lock_bytes)
            .map_err(|error| invalid(format!("invalid distribution lock: {error}")))?;
        validate_lock_identity(&lock)?;
        validate_current_platform(&lock)?;
        if lock.files.len() > 2048 {
            return Err(invalid("distribution declares more than 2048 files"));
        }
        let mut files = BTreeMap::new();
        for file in &lock.files {
            validate_relative(&file.path)?;
            validate_digest(&file.sha256)?;
            if file.role.trim().is_empty() {
                return Err(invalid(format!(
                    "distribution file `{}` has no role",
                    file.path
                )));
            }
            if files.insert(file.path.clone(), file.clone()).is_some() {
                return Err(invalid(format!(
                    "duplicate distribution path `{}`",
                    file.path
                )));
            }
            verify_file(&root, file)?;
        }
        require_role_path(&files, "host_authority", ".lenso/host-build.json")?;
        require_role_path(&files, "bundle_inventory", "bundles.json")?;
        require_role_path(&files, "host_runtime", "runtime/lenso-host-runtime")?;
        require_role_path(&files, "runtime_resolver", "runtime/lenso-resolver")?;
        require_role_path(&files, "process_owner", "runtime/lenso-process-owner")?;
        require_role_path(&files, "entrypoint", "host.js")?;
        require_role_path(&files, "notices", "THIRD_PARTY_NOTICES.txt")?;
        Ok(Self {
            root,
            identity: sha256(&lock_bytes),
            lock,
            files,
        })
    }

    /// Returns the exact digest of the distribution lock bytes.
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// Returns the App identity fixed by the distribution.
    pub fn app_id(&self) -> &str {
        &self.lock.app_id
    }

    /// Returns the canonical directory containing this verified distribution.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves one external Plugin Root and closes its Plan over verified Artifacts.
    pub fn resolve(
        &self,
        app_root: impl AsRef<Path>,
    ) -> Result<PreparedHostGeneration, DistributionError> {
        let app_root = fs::canonicalize(app_root.as_ref())
            .map_err(|error| io_error("locate external App root", error))?;
        if !fs::metadata(&app_root)
            .map_err(|error| io_error("inspect external App root", error))?
            .is_dir()
        {
            return Err(invalid("external App root must be a directory"));
        }
        let resolution = invoke_resolver(self, &app_root)?;
        if resolution.schema != "lenso.runtime-app-resolution.v1"
            || resolution.app_id != self.lock.app_id
        {
            return Err(invalid(
                "resolver output differs from distribution App authority",
            ));
        }
        validate_digest(&resolution.authority_digest)?;
        validate_digest(&resolution.host_build_digest)?;
        validate_digest(&resolution.plugin_root_revision)?;
        resolution
            .plan
            .validate()
            .map_err(|error| invalid(format!("resolver returned invalid Plan: {error}")))?;
        let authority = self
            .files
            .get(".lenso/host-build.json")
            .expect("required above");
        if resolution.host_build_digest != authority.sha256 {
            return Err(invalid(
                "resolver Host authority digest differs from distribution lock",
            ));
        }
        let inventory_bytes = read_regular_bounded(
            &self.root.join("bundles.json"),
            INVENTORY_LIMIT,
            "bundle inventory",
        )?;
        let inventory: Vec<BundleInventory> = strict_json("bundles.json", &inventory_bytes)
            .map_err(|error| invalid(format!("invalid bundle inventory: {error}")))?;
        if inventory.len() > 256 {
            return Err(invalid("bundle inventory exceeds 256 entries"));
        }
        let staging = app_root.join(".lenso/runtime-artifacts");
        fs::create_dir_all(&staging)
            .map_err(|error| io_error("create private Artifact staging root", error))?;
        let artifacts = resolve_artifacts(self, &resolution.plan, &inventory, &staging)?;
        let (host_build, policy) = generation_authority(self, &resolution.plan)?;
        let generation = resolve_plan_generation(PlanGenerationInput {
            app_id: &resolution.app_id,
            authority_digest: &resolution.authority_digest,
            plan: &resolution.plan,
            host_build: &host_build,
            policy: &policy,
            artifacts,
            resources: InstanceResourceCatalog::new(),
        })
        .map_err(|error| invalid(format!("close resolved Generation: {error}")))?;
        Ok(PreparedHostGeneration {
            distribution_identity: self.identity.clone(),
            plugin_root_revision: resolution.plugin_root_revision,
            generation,
        })
    }
}

/// One executable Generation produced from a verified distribution and external Root.
#[derive(Clone, Debug)]
pub struct PreparedHostGeneration {
    pub distribution_identity: String,
    pub plugin_root_revision: String,
    pub generation: ResolvedGeneration,
}

/// Fail-closed distribution error without embedding untrusted file contents.
#[derive(Debug)]
pub struct DistributionError(String);

impl fmt::Display for DistributionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for DistributionError {}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DistributionLock {
    schema: String,
    app_id: String,
    target: String,
    platform: String,
    arch: String,
    files: Vec<DistributionFile>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DistributionFile {
    path: String,
    role: String,
    sha256: String,
    size: u64,
    executable: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeResolution {
    schema: String,
    app_id: String,
    authority_digest: String,
    host_build_digest: String,
    plugin_root_revision: String,
    plan: ResolvedAppPlan,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleInventory {
    path: String,
    plugin_id: String,
    release_version: String,
    manifest_digest: String,
    execution_class: String,
    target: String,
    implementation_id: String,
    artifact_path: String,
    artifact_digest: String,
    artifact_size: u64,
    artifact_media_type: String,
    artifact_target: String,
}

fn validate_lock_identity(lock: &DistributionLock) -> Result<(), DistributionError> {
    if lock.schema != "lenso.host-distribution.v1"
        || lock.app_id.trim().is_empty()
        || lock.target.trim().is_empty()
    {
        return Err(invalid("invalid distribution identity"));
    }
    Ok(())
}

fn validate_current_platform(lock: &DistributionLock) -> Result<(), DistributionError> {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        value => value,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        value => value,
    };
    if lock.platform != platform || lock.arch != arch {
        return Err(invalid(format!(
            "distribution targets {}/{}, current Host is {platform}/{arch}",
            lock.platform, lock.arch
        )));
    }
    Ok(())
}

fn require_role_path(
    files: &BTreeMap<String, DistributionFile>,
    role: &str,
    path: &str,
) -> Result<(), DistributionError> {
    let matches = files
        .values()
        .filter(|file| file.role == role)
        .collect::<Vec<_>>();
    if matches.len() != 1 || matches[0].path != path {
        return Err(invalid(format!(
            "distribution needs exactly one `{role}` at `{path}`"
        )));
    }
    Ok(())
}

fn verify_file(root: &Path, expected: &DistributionFile) -> Result<(), DistributionError> {
    let path = root.join(&expected.path);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| io_error(format!("inspect `{}`", expected.path), error))?;
    if !metadata.file_type().is_file() || metadata.len() != expected.size {
        return Err(invalid(format!(
            "distribution file `{}` differs from lock",
            expected.path
        )));
    }
    #[cfg(unix)]
    if expected.executable {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(invalid(format!(
                "distribution file `{}` is not executable",
                expected.path
            )));
        }
    }
    let actual = digest_file(&path)?;
    if actual != expected.sha256 {
        return Err(invalid(format!(
            "distribution file `{}` failed integrity",
            expected.path
        )));
    }
    Ok(())
}

fn invoke_resolver(
    distribution: &VerifiedDistribution,
    app_root: &Path,
) -> Result<RuntimeResolution, DistributionError> {
    let root = app_root
        .to_str()
        .ok_or_else(|| invalid("external App root is not UTF-8"))?;
    let host_build = distribution.root.join(".lenso/host-build.json");
    let host_build = host_build
        .to_str()
        .ok_or_else(|| invalid("distribution path is not UTF-8"))?;
    let mut child = Command::new(distribution.root.join("runtime/lenso-resolver"))
        .args([
            "app",
            "show",
            "--root",
            root,
            "--host-build",
            host_build,
            "--runtime-json",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| io_error("start bundled resolver", error))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let stdout_task = thread::spawn(move || drain_bounded(stdout, RESOLVER_OUTPUT_LIMIT));
    let stderr_task = thread::spawn(move || drain_bounded(stderr, RESOLVER_ERROR_LIMIT));
    let deadline = Instant::now() + RESOLVER_TIMEOUT;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| io_error("wait for bundled resolver", error))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(invalid("bundled resolver timed out"));
        }
        thread::sleep(Duration::from_millis(10));
    };
    let (stdout, stdout_overflow) = stdout_task
        .join()
        .map_err(|_| invalid("resolver stdout reader failed"))??;
    let (stderr, stderr_overflow) = stderr_task
        .join()
        .map_err(|_| invalid("resolver stderr reader failed"))??;
    if stdout_overflow || stderr_overflow {
        return Err(invalid("bundled resolver exceeded its output limit"));
    }
    if !status.success() {
        return Err(invalid(format!(
            "bundled resolver rejected App root: {}",
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    strict_json("runtime resolution", &stdout)
        .map_err(|error| invalid(format!("bundled resolver returned invalid JSON: {error}")))
}

fn drain_bounded(
    mut reader: impl Read,
    limit: usize,
) -> Result<(Vec<u8>, bool), DistributionError> {
    let mut kept = Vec::new();
    let mut overflow = false;
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| io_error("read resolver output", error))?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(kept.len());
        kept.extend_from_slice(&buffer[..count.min(remaining)]);
        overflow |= count > remaining;
    }
    Ok((kept, overflow))
}

fn resolve_artifacts(
    distribution: &VerifiedDistribution,
    plan: &ResolvedAppPlan,
    inventory: &[BundleInventory],
    staging: &Path,
) -> Result<Vec<PlanArtifact>, DistributionError> {
    let mut result = Vec::new();
    for instance in plan.plugin_instances() {
        let candidates = inventory
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                item.plugin_id == instance.package_id()
                    && item.release_version == instance.package_revision()
                    && item.execution_class == instance.execution_class().as_str()
                    && item.target == distribution.lock.target
            })
            .collect::<Vec<_>>();
        let Some((index, selected)) = candidates.first().copied() else {
            return Err(invalid(format!(
                "no distribution Artifact for Plugin Instance `{}`",
                instance.instance_key()
            )));
        };
        if candidates
            .iter()
            .skip(1)
            .any(|(_, item)| artifact_identity(item) != artifact_identity(selected))
        {
            return Err(invalid(format!(
                "ambiguous distribution Artifact for Plugin Instance `{}`",
                instance.instance_key()
            )));
        }
        validate_digest(&selected.manifest_digest)?;
        validate_digest(&selected.artifact_digest)?;
        validate_relative(&selected.path)?;
        validate_relative(&selected.artifact_path)?;
        let bundle = distribution.files.get(&selected.path).ok_or_else(|| {
            invalid(format!(
                "selected bundle `{}` is absent from distribution lock",
                selected.path
            ))
        })?;
        if bundle.role != "plugin_bundle" {
            return Err(invalid(format!(
                "selected bundle `{}` differs from inventory",
                selected.path
            )));
        }
        if selected.implementation_id.trim().is_empty()
            || selected.artifact_media_type.trim().is_empty()
            || selected.artifact_target.trim().is_empty()
        {
            return Err(invalid("selected Artifact identity is incomplete"));
        }
        let name = Path::new(&selected.artifact_path)
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| invalid("selected Artifact needs a UTF-8 filename"))?;
        let relative = format!("artifacts/{index}/{name}");
        let locked = distribution.files.get(&relative).ok_or_else(|| {
            invalid(format!(
                "selected Artifact `{relative}` is absent from distribution lock"
            ))
        })?;
        if locked.role != "plugin_artifact"
            || locked.sha256 != selected.artifact_digest
            || locked.size != selected.artifact_size
        {
            return Err(invalid(format!(
                "selected Artifact `{relative}` differs from inventory"
            )));
        }
        let handle = ArtifactHandle::open_with_staging_root(
            distribution.root.join(&relative),
            &selected.artifact_digest,
            selected.artifact_size,
            staging,
        )
        .map_err(|error| invalid(format!("open selected Artifact `{relative}`: {error:?}")))?;
        result.push(PlanArtifact {
            instance_key: instance.instance_key().to_owned(),
            plugin_id: selected.plugin_id.clone(),
            artifact_id: selected.implementation_id.clone(),
            media_type: selected.artifact_media_type.clone(),
            target: selected.artifact_target.clone(),
            handle,
        });
    }
    Ok(result)
}

fn artifact_identity(item: &BundleInventory) -> (&str, &str, u64, &str, &str, &str) {
    (
        &item.implementation_id,
        &item.artifact_digest,
        item.artifact_size,
        &item.artifact_media_type,
        &item.artifact_target,
        &item.artifact_path,
    )
}

fn generation_authority(
    distribution: &VerifiedDistribution,
    plan: &ResolvedAppPlan,
) -> Result<
    (
        CanonicalDocument<HostBuildManifest>,
        CanonicalDocument<HostExecutionPolicy>,
    ),
    DistributionError,
> {
    let runtime = distribution
        .files
        .get("runtime/lenso-host-runtime")
        .expect("required above");
    let classes = plan
        .plugin_instances()
        .iter()
        .map(|instance| instance.execution_class().as_str().to_owned())
        .collect::<BTreeSet<_>>();
    let adapter_profiles = classes
        .iter()
        .map(|execution_class| {
            let adapter_build_identity = if execution_class == "lenso.bun-process@1" {
                let bun = distribution.files.get("runtime/bun").ok_or_else(|| {
                    invalid("Bun execution requires `javascript_runtime` at `runtime/bun`")
                })?;
                if bun.role != "javascript_runtime" {
                    return Err(invalid(
                        "Bun execution requires `javascript_runtime` at `runtime/bun`",
                    ));
                }
                sha256(format!("{}\n{}", runtime.sha256, bun.sha256).as_bytes())
            } else {
                runtime.sha256.clone()
            };
            Ok(AdapterProfile {
                execution_class: execution_class.clone(),
                adapter_build_identity,
                targets: vec![distribution.lock.target.clone()],
                profiles: vec!["request".to_owned()],
            })
        })
        .collect::<Result<Vec<_>, DistributionError>>()?;
    let host_build = CanonicalDocument::from_value(
        "lenso-host-build.json",
        HostBuildManifest {
            schema_version: 3,
            app_id: distribution.lock.app_id.clone(),
            host_executable_digest: runtime.sha256.clone(),
            target: distribution.lock.target.clone(),
            embedded_plugins: Vec::new(),
            adapter_profiles,
        },
    )
    .map_err(|error| invalid(format!("construct Host build authority: {error}")))?;
    let policy = CanonicalDocument::from_value(
        "lenso-host-execution-policy.json",
        HostExecutionPolicy {
            schema_version: 2,
            app_id: distribution.lock.app_id.clone(),
            host_build_manifest_digest: host_build.digest().to_owned(),
            target: distribution.lock.target.clone(),
            preference: classes.into_iter().collect(),
        },
    )
    .map_err(|error| invalid(format!("construct Host execution policy: {error}")))?;
    Ok((host_build, policy))
}

fn read_regular_bounded(
    path: &Path,
    limit: u64,
    label: &str,
) -> Result<Vec<u8>, DistributionError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| io_error(format!("inspect {label}"), error))?;
    if !metadata.file_type().is_file() || metadata.len() > limit {
        return Err(invalid(format!(
            "{label} must be a regular file no larger than {limit} bytes"
        )));
    }
    fs::read(path).map_err(|error| io_error(format!("read {label}"), error))
}

fn digest_file(path: &Path) -> Result<String, DistributionError> {
    let mut file = File::open(path).map_err(|error| io_error("open distribution file", error))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| io_error("hash distribution file", error))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn validate_digest(value: &str) -> Result<(), DistributionError> {
    if value.len() != 71
        || !value.starts_with("sha256:")
        || !value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(invalid("invalid SHA-256 digest"));
    }
    Ok(())
}

fn validate_relative(value: &str) -> Result<(), DistributionError> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains('\\')
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid(format!(
            "invalid distribution-relative path `{value}`"
        )));
    }
    Ok(())
}

fn invalid(detail: impl Into<String>) -> DistributionError {
    DistributionError(detail.into())
}

fn io_error(context: impl fmt::Display, error: impl fmt::Display) -> DistributionError {
    DistributionError(format!("{context}: {error}"))
}

#[cfg(test)]
mod tests;
