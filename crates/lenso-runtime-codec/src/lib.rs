//! Shared artifact and generated Capability codec seams for Execution Adapters.

use std::{
    any::Any,
    collections::BTreeMap,
    env, fs,
    io::Write as _,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use lenso_app_plan::{
    CapabilityCardinality, ExecutionClassId, PluginInstancePlan, ResolvedAppPlan,
};
use lenso_kernel::{
    InvocationContext, NativeRequestEndpoint, NativeStream, NativeStreamEndpoint, NativeStreamItem,
    NativeStreamSession, PluginDependencies, PluginDependencyHandle, PluginStreamDependencyHandle,
    PreparedBinding, PreparedNativeApp, PreparedNativePlugin, PreparedStreamBinding,
    RuntimeFailure, StreamCapability, StreamEvent,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Immutable, content-addressed files owned by one Plugin Instance Generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstanceResources {
    digest: String,
    total_size: u64,
    files: BTreeMap<String, Arc<[u8]>>,
}

impl Default for InstanceResources {
    fn default() -> Self {
        Self::from_files([]).expect("the empty resource snapshot is valid")
    }
}

impl InstanceResources {
    /// Builds one deterministic snapshot from normalized relative paths and owned bytes.
    pub fn from_files(
        files: impl IntoIterator<Item = (String, Vec<u8>)>,
    ) -> Result<Self, RuntimeFailure> {
        let mut indexed = BTreeMap::<String, Arc<[u8]>>::new();
        let mut total_size = 0_u64;
        for (path, bytes) in files {
            validate_resource_path(&path)?;
            total_size = total_size
                .checked_add(
                    u64::try_from(bytes.len())
                        .map_err(|_| invalid_resources("resource file is too large"))?,
                )
                .ok_or_else(|| invalid_resources("resource snapshot size overflow"))?;
            if indexed.insert(path.clone(), Arc::from(bytes)).is_some() {
                return Err(invalid_resources(format!(
                    "duplicate Plugin resource path `{path}`"
                )));
            }
        }
        let mut hasher = Sha256::new();
        hasher.update(b"lenso.instance-resources@1\0");
        for (path, bytes) in &indexed {
            hasher.update(
                u64::try_from(path.len())
                    .expect("path length fits u64")
                    .to_be_bytes(),
            );
            hasher.update(path.as_bytes());
            hasher.update(
                u64::try_from(bytes.len())
                    .expect("content length fits u64")
                    .to_be_bytes(),
            );
            hasher.update(bytes.as_ref());
        }
        Ok(Self {
            digest: format!("sha256:{}", hex::encode(hasher.finalize())),
            total_size,
            files: indexed,
        })
    }

    /// Returns the deterministic identity of every path and byte in this snapshot.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Returns the number of snapshotted files.
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Returns the aggregate byte size.
    pub const fn total_size(&self) -> u64 {
        self.total_size
    }

    /// Lists normalized paths in deterministic order.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    /// Reads one immutable resource without consulting the live filesystem.
    pub fn read(&self, path: &str) -> Result<&[u8], RuntimeFailure> {
        validate_resource_path(path)?;
        self.files
            .get(path)
            .map(AsRef::as_ref)
            .ok_or_else(|| invalid_resources(format!("Plugin resource `{path}` was not found")))
    }

    /// Reads one immutable UTF-8 resource.
    pub fn read_text(&self, path: &str) -> Result<&str, RuntimeFailure> {
        std::str::from_utf8(self.read(path)?)
            .map_err(|_| invalid_resources(format!("Plugin resource `{path}` is not UTF-8")))
    }
}

/// Immutable Instance-to-resource-snapshot mapping injected by the Generation Supervisor.
#[derive(Clone, Debug, Default)]
pub struct InstanceResourceCatalog {
    snapshots: BTreeMap<String, InstanceResources>,
    empty: InstanceResources,
}

impl InstanceResourceCatalog {
    /// Creates an empty catalog.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one exact Instance snapshot and rejects duplicate authority.
    pub fn with_resources(
        mut self,
        instance_key: impl Into<String>,
        resources: InstanceResources,
    ) -> Result<Self, RuntimeFailure> {
        let instance_key = instance_key.into();
        if self
            .snapshots
            .insert(instance_key.clone(), resources)
            .is_some()
        {
            return Err(invalid_resources(format!(
                "duplicate resource authority for Instance `{instance_key}`"
            )));
        }
        Ok(self)
    }

    /// Returns the selected snapshot or an immutable empty snapshot.
    pub fn for_instance(&self, instance_key: &str) -> &InstanceResources {
        self.snapshots.get(instance_key).unwrap_or(&self.empty)
    }

    /// Iterates selected Instance snapshots in deterministic order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &InstanceResources)> {
        self.snapshots
            .iter()
            .map(|(instance, resources)| (instance.as_str(), resources))
    }
}

/// Digest-verified, read-only execution input selected before Adapter preparation.
#[derive(Debug)]
struct ArtifactBacking {
    path: PathBuf,
    _directory: tempfile::TempDir,
}

/// One immutable content snapshot captured during Artifact admission.
#[derive(Clone)]
pub struct ArtifactHandle {
    source_path: PathBuf,
    backing: Arc<ArtifactBacking>,
    digest: String,
    size: u64,
}

impl std::fmt::Debug for ArtifactHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ArtifactHandle")
            .field("source_path", &self.source_path)
            .field("path", &self.backing.path)
            .field("digest", &self.digest)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl PartialEq for ArtifactHandle {
    fn eq(&self, other: &Self) -> bool {
        self.source_path == other.source_path
            && self.digest == other.digest
            && self.size == other.size
    }
}

impl Eq for ArtifactHandle {}

impl ArtifactHandle {
    /// Verifies one regular file and snapshots it in a process-private directory selected by the
    /// Host's system-temporary policy, independently of the source path. Strict isolation or
    /// path-based execution on a no-exec temporary filesystem should use
    /// [`Self::open_with_staging_root`] with a Host-owned executable root.
    pub fn open(
        path: impl Into<PathBuf>,
        expected_digest: &str,
        expected_size: u64,
    ) -> Result<Self, RuntimeFailure> {
        Self::open_inner(path.into(), expected_digest, expected_size, None)
    }

    /// Verifies one Artifact and places its private stable copy under an explicit Host-owned root.
    /// Process-capable Hosts should select a root on a filesystem that permits execution.
    pub fn open_with_staging_root(
        path: impl Into<PathBuf>,
        expected_digest: &str,
        expected_size: u64,
        staging_root: impl AsRef<Path>,
    ) -> Result<Self, RuntimeFailure> {
        Self::open_inner(
            path.into(),
            expected_digest,
            expected_size,
            Some(staging_root.as_ref()),
        )
    }

    fn open_inner(
        path: PathBuf,
        expected_digest: &str,
        expected_size: u64,
        staging_root: Option<&Path>,
    ) -> Result<Self, RuntimeFailure> {
        validate_digest(expected_digest)?;
        let path = absolute_path(path)?;
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| invalid_artifact(&path, error))?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: format!("Artifact `{}` is not a regular file", path.display()),
            });
        }
        if metadata.len() != expected_size {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: format!(
                    "Artifact `{}` size mismatch: expected {expected_size}, got {}",
                    path.display(),
                    metadata.len()
                ),
            });
        }
        let mut source = fs::File::open(&path).map_err(|error| invalid_artifact(&path, error))?;
        let opened_metadata = source
            .metadata()
            .map_err(|error| invalid_artifact(&path, error))?;
        if !opened_metadata.is_file() || opened_metadata.len() != expected_size {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: format!("Artifact `{}` changed during admission", path.display()),
            });
        }
        let (backing, actual_digest, actual_size) =
            materialize_stable_artifact(&path, &mut source, &opened_metadata, staging_root)?;
        if actual_size != expected_size {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: format!("Artifact `{}` changed during admission", path.display()),
            });
        }
        if actual_digest != expected_digest {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: format!("Artifact `{}` digest mismatch", path.display()),
            });
        }
        Ok(Self {
            source_path: path,
            backing: Arc::new(backing),
            digest: actual_digest,
            size: opened_metadata.len(),
        })
    }

    /// Returns the private stable copy containing the bytes admitted by this Handle.
    /// It is never serialized into a Plan.
    pub fn path(&self) -> &Path {
        &self.backing.path
    }

    /// Returns the original machine-local selection path for relative resource policy.
    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    /// Returns the verified content identity.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Returns the verified byte size.
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// Returns the exact bytes captured during admission.
    pub fn read_verified(&self) -> Result<Vec<u8>, RuntimeFailure> {
        let bytes = fs::read(&self.backing.path)
            .map_err(|error| invalid_artifact(&self.backing.path, error))?;
        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
        if size != self.size || digest != self.digest {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: format!(
                    "stable Artifact `{}` changed after admission",
                    self.backing.path.display()
                ),
            });
        }
        Ok(bytes)
    }
}

fn absolute_path(path: PathBuf) -> Result<PathBuf, RuntimeFailure> {
    if path.is_absolute() {
        return Ok(path);
    }
    env::current_dir()
        .map(|current| current.join(path))
        .map_err(|error| invalid_artifact(Path::new("."), error))
}

fn materialize_stable_artifact(
    source_path: &Path,
    source: &mut fs::File,
    source_metadata: &fs::Metadata,
    staging_root: Option<&Path>,
) -> Result<(ArtifactBacking, String, u64), RuntimeFailure> {
    let directory = stable_artifact_directory(source_path, staging_root)?;
    let file_name = source_path
        .file_name()
        .ok_or_else(|| RuntimeFailure::InvalidResolvedPlan {
            detail: format!("Artifact `{}` has no filename", source_path.display()),
        })?;
    let stable_path = directory.path().join(file_name);
    let mut stable = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&stable_path)
        .map_err(|error| invalid_artifact(source_path, error))?;
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = std::io::Read::read(source, &mut buffer)
            .map_err(|error| invalid_artifact(source_path, error))?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(u64::try_from(read).expect("buffer length fits u64"))
            .ok_or_else(|| RuntimeFailure::InvalidResolvedPlan {
                detail: format!("Artifact `{}` size overflow", source_path.display()),
            })?;
        hasher.update(&buffer[..read]);
        stable
            .write_all(&buffer[..read])
            .map_err(|error| invalid_artifact(source_path, error))?;
    }
    set_stable_permissions(&stable_path, source_metadata)
        .map_err(|error| invalid_artifact(source_path, error))?;
    Ok((
        ArtifactBacking {
            path: stable_path,
            _directory: directory,
        },
        format!("sha256:{}", hex::encode(hasher.finalize())),
        size,
    ))
}

fn stable_artifact_directory(
    source_path: &Path,
    staging_root: Option<&Path>,
) -> Result<tempfile::TempDir, RuntimeFailure> {
    let builder = || {
        let mut builder = tempfile::Builder::new();
        builder.prefix("lenso-artifact-");
        builder
    };
    if let Some(root) = staging_root {
        return builder()
            .tempdir_in(root)
            .map_err(|error| invalid_artifact(source_path, error));
    }
    builder()
        .tempdir()
        .map_err(|error| invalid_artifact(source_path, error))
}

#[cfg(unix)]
fn set_stable_permissions(path: &Path, metadata: &fs::Metadata) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let mode = metadata.permissions().mode() & 0o555;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_stable_permissions(path: &Path, metadata: &fs::Metadata) -> std::io::Result<()> {
    let mut permissions = metadata.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions)
}

/// Immutable Instance-to-Artifact mapping injected by the Generation Supervisor.
#[derive(Clone, Debug, Default)]
pub struct ArtifactCatalog(BTreeMap<String, ArtifactHandle>);

impl ArtifactCatalog {
    /// Creates an empty catalog for an Adapter with no selected Instances.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one exact execution input and rejects duplicate Instance authority.
    pub fn with_artifact(
        mut self,
        instance_key: impl Into<String>,
        artifact: ArtifactHandle,
    ) -> Result<Self, RuntimeFailure> {
        let instance_key = instance_key.into();
        if self.0.insert(instance_key.clone(), artifact).is_some() {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: format!("duplicate Artifact authority for Instance `{instance_key}`"),
            });
        }
        Ok(self)
    }

    /// Resolves the one selected execution input for an Instance.
    pub fn require(&self, instance_key: &str) -> Result<&ArtifactHandle, RuntimeFailure> {
        self.0
            .get(instance_key)
            .ok_or_else(|| RuntimeFailure::InvalidResolvedPlan {
                detail: format!("no admitted Artifact for Instance `{instance_key}`"),
            })
    }
}

/// Generated typed-value bridge shared by byte-oriented Execution Adapters.
pub trait JsonCapabilityCodec: std::fmt::Debug + 'static {
    /// Stable Capability series identity.
    fn capability_id(&self) -> &'static str;
    /// Exact Descriptor version.
    fn descriptor_version(&self) -> &'static str;
    /// Canonical digest of the exact generated Descriptor.
    ///
    /// Codecs generated before this method existed return an empty value and
    /// remain usable by legacy profiles. Authoring V2 profiles must reject an
    /// empty or malformed digest before readiness.
    fn descriptor_digest(&self) -> &'static str {
        ""
    }
    /// Exact request Operation table.
    fn request_operations(&self) -> &'static [&'static str];
    /// Exact bidirectional stream Operation table.
    fn stream_operations(&self) -> &'static [&'static str] {
        &[]
    }
    /// Converts one generated request into validated portable JSON.
    fn encode_request(&self, operation: &str, request: &dyn Any) -> Result<Value, RuntimeFailure>;
    /// Converts portable JSON into the generated response value.
    fn decode_response(
        &self,
        operation: &str,
        value: Value,
    ) -> Result<Box<dyn Any>, RuntimeFailure>;
    /// Converts portable JSON into the generated Domain Error value.
    fn decode_domain_error(
        &self,
        operation: &str,
        value: Value,
    ) -> Result<Box<dyn Any>, RuntimeFailure>;
    /// Converts one generated stream-open request into validated portable JSON.
    fn encode_stream_open(
        &self,
        operation: &str,
        request: &dyn Any,
    ) -> Result<Value, RuntimeFailure> {
        let _ = request;
        Err(unknown_operation(self.capability_id(), operation))
    }
    /// Converts one generated outbound stream message into validated portable JSON.
    fn encode_stream_message(
        &self,
        operation: &str,
        message: &dyn Any,
    ) -> Result<Value, RuntimeFailure> {
        let _ = message;
        Err(unknown_operation(self.capability_id(), operation))
    }
    /// Converts one portable JSON stream message into its generated value.
    fn decode_stream_message(
        &self,
        operation: &str,
        value: Value,
    ) -> Result<Box<dyn Any>, RuntimeFailure> {
        let _ = value;
        Err(unknown_operation(self.capability_id(), operation))
    }
    /// Converts one portable JSON stream terminal error into its generated value.
    fn decode_stream_domain_error(
        &self,
        operation: &str,
        value: Value,
    ) -> Result<Box<dyn Any>, RuntimeFailure> {
        let _ = value;
        Err(unknown_operation(self.capability_id(), operation))
    }
    /// Invokes one exact Plan-bound host Request dependency from portable JSON.
    fn invoke_host_request(
        &self,
        dependency: PluginDependencyHandle,
        operation: String,
        request: Value,
        context: InvocationContext,
    ) -> JsonHostRequestFuture {
        let _ = (dependency, request, context);
        Box::pin(futures::future::ready(Err(unknown_operation(
            self.capability_id(),
            &operation,
        ))))
    }
    /// Opens one exact Plan-bound host Stream dependency from portable JSON.
    fn open_host_stream(
        &self,
        dependency: PluginStreamDependencyHandle,
        operation: String,
        request: Value,
        context: InvocationContext,
    ) -> JsonHostStreamOpenFuture {
        let _ = (dependency, request, context);
        Box::pin(futures::future::ready(Err(unknown_operation(
            self.capability_id(),
            &operation,
        ))))
    }
}

/// Exact host outcome returned by a byte-oriented Plugin invocation.
#[derive(Debug)]
pub enum JsonInvocationOutcome {
    /// Successful generated response value.
    Success(Value),
    /// Declared generated Domain Error value.
    DomainError(Value),
}

/// Projects a Runtime Failure into a bounded, secret-free guest ABI value.
pub fn json_runtime_failure(error: &RuntimeFailure) -> Value {
    match error {
        RuntimeFailure::Unavailable { capability } => serde_json::json!({
            "kind": "unavailable",
            "capability": capability,
        }),
        RuntimeFailure::UnknownOperation {
            capability,
            operation,
        } => serde_json::json!({
            "kind": "unknown_operation",
            "capability": capability,
            "operation": operation,
        }),
        RuntimeFailure::AmbiguousBinding {
            capability,
            providers,
        } => serde_json::json!({
            "kind": "ambiguous_binding",
            "capability": capability,
            "providers": providers,
        }),
        RuntimeFailure::ProtocolViolation { capability } => serde_json::json!({
            "kind": "protocol_violation",
            "capability": capability,
        }),
        RuntimeFailure::AdmissionClosed => serde_json::json!({ "kind": "admission_closed" }),
        RuntimeFailure::ResourceExhausted {
            capability,
            operation,
        } => serde_json::json!({
            "kind": "resource_exhausted",
            "capability": capability,
            "operation": operation,
        }),
        RuntimeFailure::DeadlineExceeded { request_id } => serde_json::json!({
            "kind": "deadline_exceeded",
            "request_id": request_id.to_string(),
        }),
        RuntimeFailure::Cancelled { request_id } => serde_json::json!({
            "kind": "cancelled",
            "request_id": request_id.to_string(),
        }),
        RuntimeFailure::MissingPluginFactory { .. }
        | RuntimeFailure::UnavailableExecutionClass { .. }
        | RuntimeFailure::InvalidResolvedPlan { .. }
        | RuntimeFailure::Internal { .. }
        | RuntimeFailure::PluginFailure { .. }
        | RuntimeFailure::PluginRestartExhausted { .. } => {
            serde_json::json!({ "kind": "internal" })
        }
    }
}

/// Encodes a host import Request result into the stable guest envelope.
pub fn json_host_invocation_envelope(
    outcome: Result<JsonInvocationOutcome, RuntimeFailure>,
) -> Value {
    match outcome {
        Ok(JsonInvocationOutcome::Success(value)) => serde_json::json!({ "ok": value }),
        Ok(JsonInvocationOutcome::DomainError(value)) => serde_json::json!({ "error": value }),
        Err(error) => serde_json::json!({ "runtime": json_runtime_failure(&error) }),
    }
}

/// Result of one Plan-bound host Request import after generated value translation.
pub type JsonHostRequestFuture =
    futures::future::LocalBoxFuture<'static, Result<JsonInvocationOutcome, RuntimeFailure>>;

/// Adapter-neutral host Stream session exposed to a byte-oriented guest.
pub trait JsonHostStreamSession: std::fmt::Debug + 'static {
    fn send(
        self: Rc<Self>,
        message: Value,
    ) -> futures::future::LocalBoxFuture<'static, Result<(), RuntimeFailure>>;
    fn receive(
        self: Rc<Self>,
    ) -> futures::future::LocalBoxFuture<'static, Result<JsonStreamItem, RuntimeFailure>>;
    fn close_send(
        self: Rc<Self>,
    ) -> futures::future::LocalBoxFuture<'static, Result<(), RuntimeFailure>>;
    fn cancel(&self);
}

/// Result of opening one Plan-bound host Stream import.
pub type JsonHostStreamOpenFuture = futures::future::LocalBoxFuture<
    'static,
    Result<Result<Rc<dyn JsonHostStreamSession>, Value>, RuntimeFailure>,
>;

type DecodeStreamMessage<C> =
    Rc<dyn Fn(Value) -> Result<<C as StreamCapability>::Message, RuntimeFailure>>;
type EncodeStreamMessage<C> =
    Rc<dyn Fn(<C as StreamCapability>::Message) -> Result<Value, RuntimeFailure>>;
type EncodeStreamError<C> =
    Rc<dyn Fn(<C as StreamCapability>::DomainError) -> Result<Value, RuntimeFailure>>;

/// Wraps one generated typed host Stream as portable JSON for a guest import.
pub fn json_host_stream<C: StreamCapability>(
    stream: NativeStream<C>,
    decode_message: impl Fn(Value) -> Result<C::Message, RuntimeFailure> + 'static,
    encode_message: impl Fn(C::Message) -> Result<Value, RuntimeFailure> + 'static,
    encode_error: impl Fn(C::DomainError) -> Result<Value, RuntimeFailure> + 'static,
) -> Rc<dyn JsonHostStreamSession> {
    Rc::new(TypedJsonHostStream {
        stream: Rc::new(stream),
        decode_message: Rc::new(decode_message),
        encode_message: Rc::new(encode_message),
        encode_error: Rc::new(encode_error),
    })
}

struct TypedJsonHostStream<C: StreamCapability> {
    stream: Rc<NativeStream<C>>,
    decode_message: DecodeStreamMessage<C>,
    encode_message: EncodeStreamMessage<C>,
    encode_error: EncodeStreamError<C>,
}

impl<C: StreamCapability> std::fmt::Debug for TypedJsonHostStream<C> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TypedJsonHostStream")
            .field("capability", &C::ID)
            .finish_non_exhaustive()
    }
}

impl<C: StreamCapability> JsonHostStreamSession for TypedJsonHostStream<C> {
    fn send(
        self: Rc<Self>,
        message: Value,
    ) -> futures::future::LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        Box::pin(async move {
            let message = (self.decode_message)(message)?;
            self.stream.send(message).await
        })
    }

    fn receive(
        self: Rc<Self>,
    ) -> futures::future::LocalBoxFuture<'static, Result<JsonStreamItem, RuntimeFailure>> {
        Box::pin(async move {
            match self.stream.receive().await? {
                StreamEvent::Message(message) => {
                    (self.encode_message)(message).map(JsonStreamItem::Message)
                }
                StreamEvent::PeerHalfClosed => Ok(JsonStreamItem::PeerHalfClosed),
                StreamEvent::Terminal(Ok(())) => Ok(JsonStreamItem::Terminal(Ok(()))),
                StreamEvent::Terminal(Err(error)) => {
                    (self.encode_error)(error).map(|error| JsonStreamItem::Terminal(Err(error)))
                }
            }
        })
    }

    fn close_send(
        self: Rc<Self>,
    ) -> futures::future::LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        Box::pin(async move { self.stream.close_send().await })
    }

    fn cancel(&self) {
        self.stream.cancel();
    }
}

/// Stable request-only guest ABI implemented by byte-oriented Plugin runtimes.
pub const JSON_REQUEST_ABI_V1: &str = "lenso.json-request@1";

/// Stable Request and bidirectional Stream guest ABI.
pub const JSON_INTERACTIONS_ABI_V1: &str = "lenso.json-interactions@1";

/// Stable Request, Stream, and Plan-bound host Capability import ABI.
pub const JSON_HOST_IMPORTS_ABI_V1: &str = "lenso.json-host-imports@1";
/// Stable Request, Stream, and named Plan-bound Host Capability import ABI.
pub const JSON_HOST_IMPORTS_ABI_V2: &str = "lenso.json-host-imports@2";

/// Exact guest declaration returned before an Adapter opens readiness.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JsonPluginDescriptor {
    pub abi: String,
    pub capabilities: Vec<JsonCapabilityDescriptor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_capabilities: Vec<JsonRequiredCapabilityDescriptor>,
}

/// One exact request Capability exposed by a guest Plugin.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JsonCapabilityDescriptor {
    pub capability_id: String,
    pub descriptor_version: String,
    pub request_operations: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stream_operations: Vec<String>,
}

/// One exact Capability requirement declared by a guest Plugin.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JsonRequiredCapabilityDescriptor {
    pub requirement_id: String,
    pub capability_id: String,
    pub descriptor_version: String,
    pub cardinality: CapabilityCardinality,
}

/// Derives the only guest declaration accepted for one resolved Instance.
pub fn expected_json_plugin_descriptor(
    instance: &PluginInstancePlan,
) -> Result<JsonPluginDescriptor, RuntimeFailure> {
    let mut capabilities = Vec::with_capacity(instance.provided_capabilities().len());
    for descriptor in instance.provided_capabilities() {
        if !descriptor.event_operations().is_empty() {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: format!(
                    "Execution class `{}` does not support Event endpoints",
                    instance.execution_class()
                ),
            });
        }
        capabilities.push(JsonCapabilityDescriptor {
            capability_id: descriptor.capability_id().to_owned(),
            descriptor_version: descriptor.descriptor_version().to_owned(),
            request_operations: descriptor
                .request_operations()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            stream_operations: descriptor
                .stream_operations()
                .into_iter()
                .map(str::to_owned)
                .collect(),
        });
    }
    capabilities.sort();
    if capabilities
        .windows(2)
        .any(|pair| pair[0].capability_id == pair[1].capability_id)
    {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: format!(
                "Instance `{}` declares a duplicate Capability",
                instance.instance_key()
            ),
        });
    }
    let mut required_capabilities = instance
        .required_capabilities()
        .iter()
        .map(|requirement| JsonRequiredCapabilityDescriptor {
            requirement_id: requirement.requirement_id().to_owned(),
            capability_id: requirement.capability_id().to_owned(),
            descriptor_version: requirement.descriptor_version().to_owned(),
            cardinality: requirement.cardinality(),
        })
        .collect::<Vec<_>>();
    sort_required_capabilities(&mut required_capabilities);
    Ok(JsonPluginDescriptor {
        abi: if !required_capabilities.is_empty() {
            JSON_HOST_IMPORTS_ABI_V2
        } else if capabilities
            .iter()
            .any(|capability| !capability.stream_operations.is_empty())
        {
            JSON_INTERACTIONS_ABI_V1
        } else {
            JSON_REQUEST_ABI_V1
        }
        .to_owned(),
        capabilities,
        required_capabilities,
    })
}

/// Parses and compares a guest Ready declaration with exact Plan authority.
pub fn validate_json_plugin_descriptor(
    instance: &PluginInstancePlan,
    encoded: &str,
) -> Result<(), RuntimeFailure> {
    let mut actual = serde_json::from_str::<JsonPluginDescriptor>(encoded).map_err(|_| {
        RuntimeFailure::ProtocolViolation {
            capability: "lenso.json-request@1",
        }
    })?;
    actual.capabilities.sort();
    sort_required_capabilities(&mut actual.required_capabilities);
    let expected = expected_json_plugin_descriptor(instance)?;
    if actual != expected {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: format!(
                "guest descriptor does not match resolved Instance `{}`",
                instance.instance_key()
            ),
        });
    }
    Ok(())
}

fn sort_required_capabilities(requirements: &mut [JsonRequiredCapabilityDescriptor]) {
    requirements.sort_by(|left, right| {
        (
            &left.requirement_id,
            &left.capability_id,
            &left.descriptor_version,
            cardinality_order(left.cardinality),
        )
            .cmp(&(
                &right.requirement_id,
                &right.capability_id,
                &right.descriptor_version,
                cardinality_order(right.cardinality),
            ))
    });
}

const fn cardinality_order(cardinality: CapabilityCardinality) -> u8 {
    match cardinality {
        CapabilityCardinality::One => 0,
        CapabilityCardinality::Optional => 1,
        CapabilityCardinality::Many => 2,
    }
}

/// Guest transport seam shared by Wasm Component and embedded-JavaScript Adapters.
pub trait JsonRequestTransport: std::fmt::Debug + 'static {
    fn invoke(
        self: Rc<Self>,
        capability: String,
        operation: String,
        request_json: String,
        context: InvocationContext,
    ) -> futures::future::LocalBoxFuture<'static, Result<JsonInvocationOutcome, RuntimeFailure>>;
}

/// One exact transport frame received from a byte-oriented guest stream.
#[derive(Debug)]
pub enum JsonStreamItem {
    Message(Value),
    PeerHalfClosed,
    Terminal(Result<(), Value>),
}

/// Canonical portable JSON frame returned by `stream-receive` guest exports.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum JsonStreamFrame {
    Message(Value),
    PeerHalfClosed,
    TerminalSuccess,
    TerminalError(Value),
}

/// One exact Plan binding exposed to a guest Plugin after lifecycle activation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct JsonHostBindingDescriptor {
    pub binding_id: u32,
    #[serde(skip_serializing)]
    pub requirement_id: String,
    pub provider_instance: String,
    pub capability_id: String,
    pub descriptor_version: String,
    #[serde(skip_serializing)]
    pub descriptor_digest: String,
    pub request_operations: Vec<String>,
    pub stream_operations: Vec<String>,
}

#[derive(Clone)]
struct JsonHostBinding {
    descriptor: JsonHostBindingDescriptor,
    codec: Rc<dyn JsonCapabilityCodec>,
    request: Option<PluginDependencyHandle>,
    stream: Option<PluginStreamDependencyHandle>,
}

impl std::fmt::Debug for JsonHostBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JsonHostBinding")
            .field("descriptor", &self.descriptor)
            .finish_non_exhaustive()
    }
}

/// Activated, Plan-bound Capability imports for one byte-oriented guest generation.
#[derive(Debug)]
pub struct JsonHostImports {
    codecs: BTreeMap<String, Rc<dyn JsonCapabilityCodec>>,
    bindings: std::cell::RefCell<Option<Vec<JsonHostBinding>>>,
    streams: std::cell::RefCell<BTreeMap<u64, Rc<dyn JsonHostStreamSession>>>,
    next_stream_id: std::cell::Cell<u64>,
    max_streams: usize,
}

impl JsonHostImports {
    /// Creates a closed import table from the exact generated requirement codecs.
    pub fn new(
        codecs: Vec<Rc<dyn JsonCapabilityCodec>>,
        max_streams: usize,
    ) -> Result<Self, RuntimeFailure> {
        let mut by_capability = BTreeMap::new();
        for codec in codecs {
            let capability = codec.capability_id().to_owned();
            if let Some(existing) = by_capability.get(&capability) {
                if !Rc::ptr_eq(existing, &codec) {
                    return Err(RuntimeFailure::InvalidResolvedPlan {
                        detail: format!(
                            "conflicting guest import codecs for Capability `{capability}`"
                        ),
                    });
                }
            } else {
                by_capability.insert(capability, codec);
            }
        }
        Ok(Self {
            codecs: by_capability,
            bindings: std::cell::RefCell::new(None),
            streams: std::cell::RefCell::new(BTreeMap::new()),
            next_stream_id: std::cell::Cell::new(1),
            max_streams,
        })
    }

    /// Installs only the dependencies materialized from the immutable Plan.
    pub fn activate(&self, dependencies: &PluginDependencies) -> Result<(), RuntimeFailure> {
        if self.bindings.borrow().is_some() {
            return Err(RuntimeFailure::Internal {
                detail: "guest Capability imports were activated twice".to_owned(),
            });
        }
        let mut bindings = Vec::with_capacity(dependencies.len());
        for (index, dependency) in dependencies.bindings().iter().enumerate() {
            let codec = self
                .codecs
                .get(dependency.capability_id())
                .cloned()
                .ok_or_else(|| RuntimeFailure::InvalidResolvedPlan {
                    detail: format!(
                        "no generated guest import codec for Capability `{}`",
                        dependency.capability_id()
                    ),
                })?;
            let request = dependency.handle();
            let stream = dependency.stream_handle();
            validate_host_binding(&codec, request.as_ref(), stream.as_ref())?;
            let binding_id =
                u32::try_from(index).map_err(|_| RuntimeFailure::InvalidResolvedPlan {
                    detail: "guest import binding table exceeds u32 identity space".to_owned(),
                })?;
            bindings.push(JsonHostBinding {
                descriptor: JsonHostBindingDescriptor {
                    binding_id,
                    requirement_id: dependency.requirement_id().to_owned(),
                    provider_instance: dependency.provider_instance().to_owned(),
                    capability_id: dependency.capability_id().to_owned(),
                    descriptor_version: codec.descriptor_version().to_owned(),
                    descriptor_digest: codec.descriptor_digest().to_owned(),
                    request_operations: request.as_ref().map_or_else(Vec::new, |handle| {
                        handle
                            .operations()
                            .iter()
                            .map(|item| (*item).to_owned())
                            .collect()
                    }),
                    stream_operations: stream.as_ref().map_or_else(Vec::new, |handle| {
                        handle
                            .operations()
                            .iter()
                            .map(|item| (*item).to_owned())
                            .collect()
                    }),
                },
                codec,
                request,
                stream,
            });
        }
        self.bindings.replace(Some(bindings));
        Ok(())
    }

    /// Returns the exact activated binding table in resolved provider order.
    pub fn descriptors(&self) -> Result<Vec<JsonHostBindingDescriptor>, RuntimeFailure> {
        self.bindings
            .borrow()
            .as_ref()
            .map(|bindings| {
                bindings
                    .iter()
                    .map(|binding| binding.descriptor.clone())
                    .collect()
            })
            .ok_or(RuntimeFailure::AdmissionClosed)
    }

    /// Invokes one activated Request binding by its unforgeable table index.
    pub fn invoke(
        &self,
        binding_id: u32,
        operation: String,
        request: Value,
        context: InvocationContext,
    ) -> JsonHostRequestFuture {
        let binding = match self.binding(binding_id) {
            Ok(binding) => binding,
            Err(error) => return Box::pin(futures::future::ready(Err(error))),
        };
        let Some(dependency) = binding.request else {
            return Box::pin(futures::future::ready(Err(
                RuntimeFailure::UnknownOperation {
                    capability: binding.codec.capability_id(),
                    operation,
                },
            )));
        };
        binding
            .codec
            .invoke_host_request(dependency, operation, request, context)
    }

    /// Opens one activated Stream binding and assigns an Adapter-local import id.
    pub fn open_stream(
        self: Rc<Self>,
        binding_id: u32,
        operation: String,
        request: Value,
        context: InvocationContext,
    ) -> futures::future::LocalBoxFuture<'static, Result<Result<u64, Value>, RuntimeFailure>> {
        Box::pin(async move {
            if self.streams.borrow().len() >= self.max_streams {
                return Err(RuntimeFailure::ResourceExhausted {
                    capability: JSON_HOST_IMPORTS_ABI_V2,
                    operation: "stream-open".to_owned(),
                });
            }
            let binding = self.binding(binding_id)?;
            let dependency = binding
                .stream
                .ok_or_else(|| RuntimeFailure::UnknownOperation {
                    capability: binding.codec.capability_id(),
                    operation: operation.clone(),
                })?;
            match binding
                .codec
                .open_host_stream(dependency, operation, request, context)
                .await?
            {
                Ok(stream) => {
                    let stream_id = self.next_stream_id.get();
                    let next =
                        stream_id
                            .checked_add(1)
                            .ok_or(RuntimeFailure::ResourceExhausted {
                                capability: JSON_HOST_IMPORTS_ABI_V2,
                                operation: "stream-open".to_owned(),
                            })?;
                    self.next_stream_id.set(next);
                    self.streams.borrow_mut().insert(stream_id, stream);
                    Ok(Ok(stream_id))
                }
                Err(error) => Ok(Err(error)),
            }
        })
    }

    /// Sends one portable message through a guest-owned host Stream.
    pub fn send_stream(
        &self,
        stream_id: u64,
        message: Value,
    ) -> futures::future::LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        match self.stream(stream_id) {
            Ok(stream) => stream.send(message),
            Err(error) => Box::pin(futures::future::ready(Err(error))),
        }
    }

    /// Receives the next portable frame from one guest-owned host Stream.
    pub fn receive_stream(
        self: Rc<Self>,
        stream_id: u64,
    ) -> futures::future::LocalBoxFuture<'static, Result<JsonStreamItem, RuntimeFailure>> {
        Box::pin(async move {
            let stream = self.stream(stream_id)?;
            let item = stream.receive().await?;
            if matches!(item, JsonStreamItem::Terminal(_)) {
                self.streams.borrow_mut().remove(&stream_id);
            }
            Ok(item)
        })
    }

    /// Half-closes the guest-to-host direction of one guest-owned host Stream.
    pub fn close_stream_send(
        &self,
        stream_id: u64,
    ) -> futures::future::LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        match self.stream(stream_id) {
            Ok(stream) => stream.close_send(),
            Err(error) => Box::pin(futures::future::ready(Err(error))),
        }
    }

    /// Cancels and removes one guest-owned host Stream.
    pub fn cancel_stream(&self, stream_id: u64) -> Result<(), RuntimeFailure> {
        let stream = self
            .streams
            .borrow_mut()
            .remove(&stream_id)
            .ok_or_else(unknown_host_stream)?;
        stream.cancel();
        Ok(())
    }

    /// Closes admission and cancels every import Stream owned by this generation.
    pub fn deactivate(&self) {
        self.bindings.replace(None);
        for (_, stream) in std::mem::take(&mut *self.streams.borrow_mut()) {
            stream.cancel();
        }
    }

    fn binding(&self, binding_id: u32) -> Result<JsonHostBinding, RuntimeFailure> {
        let bindings = self.bindings.borrow();
        let bindings = bindings.as_ref().ok_or(RuntimeFailure::AdmissionClosed)?;
        bindings
            .get(binding_id as usize)
            .cloned()
            .ok_or(RuntimeFailure::ProtocolViolation {
                capability: JSON_HOST_IMPORTS_ABI_V2,
            })
    }

    fn stream(&self, stream_id: u64) -> Result<Rc<dyn JsonHostStreamSession>, RuntimeFailure> {
        self.streams
            .borrow()
            .get(&stream_id)
            .cloned()
            .ok_or_else(unknown_host_stream)
    }
}

fn validate_host_binding(
    codec: &Rc<dyn JsonCapabilityCodec>,
    request: Option<&PluginDependencyHandle>,
    stream: Option<&PluginStreamDependencyHandle>,
) -> Result<(), RuntimeFailure> {
    for (capability, version) in request
        .map(|handle| (handle.capability_id(), handle.descriptor_version()))
        .into_iter()
        .chain(stream.map(|handle| (handle.capability_id(), handle.descriptor_version())))
    {
        if capability != codec.capability_id() || version != codec.descriptor_version() {
            return Err(RuntimeFailure::ProtocolViolation {
                capability: codec.capability_id(),
            });
        }
    }
    Ok(())
}

fn unknown_host_stream() -> RuntimeFailure {
    RuntimeFailure::ProtocolViolation {
        capability: JSON_HOST_IMPORTS_ABI_V2,
    }
}

impl JsonStreamFrame {
    /// Parses one bounded guest result into the Adapter-neutral transport item.
    pub fn decode(
        encoded: &str,
        capability: &'static str,
    ) -> Result<JsonStreamItem, RuntimeFailure> {
        match serde_json::from_str(encoded)
            .map_err(|_| RuntimeFailure::ProtocolViolation { capability })?
        {
            Self::Message(value) => Ok(JsonStreamItem::Message(value)),
            Self::PeerHalfClosed => Ok(JsonStreamItem::PeerHalfClosed),
            Self::TerminalSuccess => Ok(JsonStreamItem::Terminal(Ok(()))),
            Self::TerminalError(value) => Ok(JsonStreamItem::Terminal(Err(value))),
        }
    }
}

/// Adapter-owned transport session for the portable JSON Stream ABI.
pub trait JsonStreamSessionTransport: std::fmt::Debug + 'static {
    fn send(
        self: Rc<Self>,
        message_json: String,
    ) -> futures::future::LocalBoxFuture<'static, Result<(), RuntimeFailure>>;
    fn receive(
        self: Rc<Self>,
    ) -> futures::future::LocalBoxFuture<'static, Result<JsonStreamItem, RuntimeFailure>>;
    fn close_send(
        self: Rc<Self>,
    ) -> futures::future::LocalBoxFuture<'static, Result<(), RuntimeFailure>>;
    fn cancel(&self);
}

/// Adapter-owned result of opening one portable JSON stream transport session.
pub type JsonStreamOpenFuture = futures::future::LocalBoxFuture<
    'static,
    Result<Result<Rc<dyn JsonStreamSessionTransport>, Value>, RuntimeFailure>,
>;

/// Guest transport seam shared by Stream-capable byte-oriented Adapters.
pub trait JsonStreamTransport: std::fmt::Debug + 'static {
    fn open(
        self: Rc<Self>,
        capability: String,
        operation: String,
        request_json: String,
        context: InvocationContext,
    ) -> JsonStreamOpenFuture;
}

/// Builds typed Kernel endpoints over one exact guest transport generation.
pub fn json_request_endpoints<T: JsonRequestTransport>(
    transport: Rc<T>,
    codecs: Vec<Rc<dyn JsonCapabilityCodec>>,
) -> Vec<Rc<dyn NativeRequestEndpoint>> {
    let transport: Rc<dyn JsonRequestTransport> = transport;
    codecs
        .into_iter()
        .filter(|codec| !codec.request_operations().is_empty())
        .map(|codec| {
            Rc::new(JsonRequestEndpoint {
                transport: transport.clone(),
                codec,
            }) as Rc<dyn NativeRequestEndpoint>
        })
        .collect()
}

/// Builds typed Kernel Stream endpoints over one exact guest transport generation.
pub fn json_stream_endpoints<T: JsonStreamTransport>(
    transport: Rc<T>,
    codecs: Vec<Rc<dyn JsonCapabilityCodec>>,
) -> Vec<Rc<dyn NativeStreamEndpoint>> {
    let transport: Rc<dyn JsonStreamTransport> = transport;
    codecs
        .into_iter()
        .filter(|codec| !codec.stream_operations().is_empty())
        .map(|codec| {
            Rc::new(JsonStreamEndpoint {
                transport: transport.clone(),
                codec,
            }) as Rc<dyn NativeStreamEndpoint>
        })
        .collect()
}

#[derive(Debug)]
struct JsonStreamEndpoint {
    transport: Rc<dyn JsonStreamTransport>,
    codec: Rc<dyn JsonCapabilityCodec>,
}

impl NativeStreamEndpoint for JsonStreamEndpoint {
    fn capability_id(&self) -> &'static str {
        self.codec.capability_id()
    }
    fn descriptor_version(&self) -> &'static str {
        self.codec.descriptor_version()
    }
    fn operations(&self) -> &'static [&'static str] {
        self.codec.stream_operations()
    }

    fn open(
        &self,
        operation: &str,
        request: Box<dyn Any>,
        context: InvocationContext,
    ) -> futures::future::LocalBoxFuture<
        'static,
        Result<Result<Box<dyn NativeStreamSession>, Box<dyn Any>>, RuntimeFailure>,
    > {
        let transport = self.transport.clone();
        let codec = self.codec.clone();
        let operation = operation.to_owned();
        Box::pin(async move {
            if !codec.stream_operations().contains(&operation.as_str()) {
                return Err(unknown_operation(codec.capability_id(), &operation));
            }
            let request = codec.encode_stream_open(&operation, request.as_ref())?;
            let request_json =
                serde_json::to_string(&request).map_err(|_| RuntimeFailure::ProtocolViolation {
                    capability: codec.capability_id(),
                })?;
            match transport
                .open(
                    codec.capability_id().to_owned(),
                    operation.clone(),
                    request_json,
                    context,
                )
                .await?
            {
                Ok(session) => Ok(Ok(Box::new(JsonStreamSession {
                    session,
                    codec,
                    operation,
                }) as Box<dyn NativeStreamSession>)),
                Err(error) => codec.decode_stream_domain_error(&operation, error).map(Err),
            }
        })
    }
}

#[derive(Debug)]
struct JsonStreamSession {
    session: Rc<dyn JsonStreamSessionTransport>,
    codec: Rc<dyn JsonCapabilityCodec>,
    operation: String,
}

impl NativeStreamSession for JsonStreamSession {
    fn send(
        &self,
        message: Box<dyn Any>,
    ) -> futures::future::LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        let encoded = self
            .codec
            .encode_stream_message(&self.operation, message.as_ref())
            .and_then(|value| {
                serde_json::to_string(&value).map_err(|_| RuntimeFailure::ProtocolViolation {
                    capability: self.codec.capability_id(),
                })
            });
        let session = self.session.clone();
        Box::pin(async move { session.send(encoded?).await })
    }

    fn receive(
        &self,
    ) -> futures::future::LocalBoxFuture<'static, Result<NativeStreamItem, RuntimeFailure>> {
        let session = self.session.clone();
        let codec = self.codec.clone();
        let operation = self.operation.clone();
        Box::pin(async move {
            match session.receive().await? {
                JsonStreamItem::Message(value) => codec
                    .decode_stream_message(&operation, value)
                    .map(NativeStreamItem::Message),
                JsonStreamItem::PeerHalfClosed => Ok(NativeStreamItem::PeerHalfClosed),
                JsonStreamItem::Terminal(Ok(())) => Ok(NativeStreamItem::Terminal(Ok(()))),
                JsonStreamItem::Terminal(Err(value)) => codec
                    .decode_stream_domain_error(&operation, value)
                    .map(|error| NativeStreamItem::Terminal(Err(error))),
            }
        })
    }

    fn close_send(&self) -> futures::future::LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        self.session.clone().close_send()
    }

    fn cancel(&self) {
        self.session.cancel();
    }
}

#[derive(Debug)]
struct JsonRequestEndpoint {
    transport: Rc<dyn JsonRequestTransport>,
    codec: Rc<dyn JsonCapabilityCodec>,
}

impl NativeRequestEndpoint for JsonRequestEndpoint {
    fn capability_id(&self) -> &'static str {
        self.codec.capability_id()
    }

    fn descriptor_version(&self) -> &'static str {
        self.codec.descriptor_version()
    }

    fn operations(&self) -> &'static [&'static str] {
        self.codec.request_operations()
    }

    fn invoke(
        &self,
        operation: &str,
        request: Box<dyn Any>,
        context: InvocationContext,
    ) -> futures::future::LocalBoxFuture<
        'static,
        Result<Result<Box<dyn Any>, Box<dyn Any>>, RuntimeFailure>,
    > {
        let transport = self.transport.clone();
        let codec = self.codec.clone();
        let operation = operation.to_owned();
        Box::pin(async move {
            if !codec.request_operations().contains(&operation.as_str()) {
                return Err(RuntimeFailure::UnknownOperation {
                    capability: codec.capability_id(),
                    operation,
                });
            }
            let request = codec.encode_request(&operation, request.as_ref())?;
            let request =
                serde_json::to_string(&request).map_err(|_| RuntimeFailure::ProtocolViolation {
                    capability: codec.capability_id(),
                })?;
            match transport
                .invoke(
                    codec.capability_id().to_owned(),
                    operation.clone(),
                    request,
                    context,
                )
                .await?
            {
                JsonInvocationOutcome::Success(value) => {
                    codec.decode_response(&operation, value).map(Ok)
                }
                JsonInvocationOutcome::DomainError(value) => {
                    codec.decode_domain_error(&operation, value).map(Err)
                }
            }
        })
    }
}

/// Validates Plan descriptors against registered generated codecs.
pub fn codecs_for_instance(
    instance: &PluginInstancePlan,
    codecs: &BTreeMap<String, Rc<dyn JsonCapabilityCodec>>,
) -> Result<Vec<Rc<dyn JsonCapabilityCodec>>, RuntimeFailure> {
    let mut selected = Vec::with_capacity(instance.provided_capabilities().len());
    for descriptor in instance.provided_capabilities() {
        if !descriptor.event_operations().is_empty() {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: format!(
                    "Execution class `{}` does not support Event endpoints",
                    instance.execution_class()
                ),
            });
        }
        let codec = codecs.get(descriptor.capability_id()).ok_or_else(|| {
            RuntimeFailure::InvalidResolvedPlan {
                detail: format!(
                    "no generated codec for Capability `{}`",
                    descriptor.capability_id()
                ),
            }
        })?;
        let request_operations: Vec<_> = codec
            .request_operations()
            .iter()
            .map(|operation| (*operation).to_owned())
            .collect();
        let stream_operations: Vec<_> = codec
            .stream_operations()
            .iter()
            .map(|operation| (*operation).to_owned())
            .collect();
        let expected_request: Vec<_> = descriptor
            .request_operations()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let expected_stream: Vec<_> = descriptor
            .stream_operations()
            .into_iter()
            .map(str::to_owned)
            .collect();
        if codec.descriptor_version() != descriptor.descriptor_version()
            || request_operations != expected_request
            || stream_operations != expected_stream
        {
            return Err(RuntimeFailure::ProtocolViolation {
                capability: codec.capability_id(),
            });
        }
        selected.push(codec.clone());
    }
    Ok(selected)
}

/// Validates every declared guest requirement against one registered generated codec.
pub fn codecs_for_requirements(
    instance: &PluginInstancePlan,
    codecs: &BTreeMap<String, Rc<dyn JsonCapabilityCodec>>,
) -> Result<Vec<Rc<dyn JsonCapabilityCodec>>, RuntimeFailure> {
    let mut selected = BTreeMap::new();
    for requirement in instance.required_capabilities() {
        let codec = codecs.get(requirement.capability_id()).ok_or_else(|| {
            RuntimeFailure::InvalidResolvedPlan {
                detail: format!(
                    "no generated guest import codec for Capability `{}`",
                    requirement.capability_id()
                ),
            }
        })?;
        if codec.descriptor_version() != requirement.descriptor_version() {
            return Err(RuntimeFailure::ProtocolViolation {
                capability: codec.capability_id(),
            });
        }
        selected
            .entry(requirement.capability_id().to_owned())
            .or_insert_with(|| codec.clone());
    }
    Ok(selected.into_values().collect())
}

/// Builds exact request bindings from Adapter-prepared Plugin generations.
pub fn prepare_request_app(
    plan: &ResolvedAppPlan,
    execution_class: &ExecutionClassId,
    generations: BTreeMap<String, PreparedNativePlugin>,
) -> Result<PreparedNativeApp, RuntimeFailure> {
    let selected_instances = plan
        .plugin_instances()
        .iter()
        .filter(|instance| instance.execution_class() == execution_class)
        .map(|instance| instance.instance_key().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    let mut endpoints = BTreeMap::new();
    let mut stream_endpoints = BTreeMap::new();
    for (instance_key, generation) in &generations {
        for endpoint in generation.endpoints() {
            let identity = (instance_key.clone(), endpoint.capability_id().to_owned());
            if endpoints.insert(identity, endpoint.clone()).is_some() {
                return Err(RuntimeFailure::InvalidResolvedPlan {
                    detail: format!("duplicate request endpoint on Instance `{instance_key}`"),
                });
            }
        }
        for endpoint in generation.stream_endpoints() {
            let identity = (instance_key.clone(), endpoint.capability_id().to_owned());
            if stream_endpoints
                .insert(identity, endpoint.clone())
                .is_some()
            {
                return Err(RuntimeFailure::InvalidResolvedPlan {
                    detail: format!("duplicate stream endpoint on Instance `{instance_key}`"),
                });
            }
        }
    }
    for instance in plan
        .plugin_instances()
        .iter()
        .filter(|instance| selected_instances.contains(instance.instance_key()))
    {
        if !generations.contains_key(instance.instance_key()) {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: format!("Adapter omitted Instance `{}`", instance.instance_key()),
            });
        }
    }
    let mut bindings = Vec::new();
    let mut stream_bindings = Vec::new();
    for binding in plan.capability_bindings() {
        let key = (
            binding.provider_instance().to_owned(),
            binding.capability_id().to_owned(),
        );
        let request_endpoint = endpoints.get(&key);
        let stream_endpoint = stream_endpoints.get(&key);
        if let Some(endpoint) = request_endpoint {
            bindings.push(
                PreparedBinding::new(
                    binding.consumer_instance(),
                    binding.provider_instance(),
                    endpoint.clone(),
                )
                .with_requirement_id(binding.requirement_id()),
            );
        }
        if let Some(endpoint) = stream_endpoint {
            stream_bindings.push(
                PreparedStreamBinding::new(
                    binding.consumer_instance(),
                    binding.provider_instance(),
                    endpoint.clone(),
                )
                .with_requirement_id(binding.requirement_id()),
            );
        }
        if request_endpoint.is_none()
            && stream_endpoint.is_none()
            && selected_instances.contains(binding.provider_instance())
        {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: format!(
                    "Adapter omitted Capability `{}` endpoint for Instance `{}`",
                    binding.capability_id(),
                    binding.provider_instance()
                ),
            });
        }
    }
    Ok(PreparedNativeApp::new(bindings, generations).with_stream_bindings(stream_bindings))
}

/// Looks up the exact codec and validates the Operation before dispatch.
pub fn require_operation(
    codecs: &BTreeMap<String, Rc<dyn JsonCapabilityCodec>>,
    capability_id: &str,
    operation: &str,
) -> Result<Rc<dyn JsonCapabilityCodec>, RuntimeFailure> {
    let codec =
        codecs
            .get(capability_id)
            .cloned()
            .ok_or_else(|| RuntimeFailure::InvalidResolvedPlan {
                detail: format!("no generated codec for Capability `{capability_id}`"),
            })?;
    if !codec.request_operations().contains(&operation) {
        return Err(RuntimeFailure::UnknownOperation {
            capability: codec.capability_id(),
            operation: operation.to_owned(),
        });
    }
    Ok(codec)
}

fn unknown_operation(capability: &'static str, operation: &str) -> RuntimeFailure {
    RuntimeFailure::UnknownOperation {
        capability,
        operation: operation.to_owned(),
    }
}

fn validate_digest(digest: &str) -> Result<(), RuntimeFailure> {
    let valid = digest.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    });
    if valid {
        Ok(())
    } else {
        Err(RuntimeFailure::InvalidResolvedPlan {
            detail: format!("invalid canonical SHA-256 digest `{digest}`"),
        })
    }
}

fn invalid_artifact(path: &Path, error: impl std::fmt::Display) -> RuntimeFailure {
    RuntimeFailure::InvalidResolvedPlan {
        detail: format!("cannot read Artifact `{}`: {error}", path.display()),
    }
}

fn validate_resource_path(path: &str) -> Result<(), RuntimeFailure> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains(['\\', '\0'])
        || path
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return Err(invalid_resources(format!(
            "invalid Plugin resource path `{path}`"
        )));
    }
    Ok(())
}

fn invalid_resources(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::InvalidResolvedPlan {
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn artifact_handle_keeps_the_admitted_bytes_after_source_drift() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"first").unwrap();
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(b"first")));
        let handle = ArtifactHandle::open(file.path(), &digest, 5).unwrap();
        file.as_file_mut().set_len(0).unwrap();
        file.write_all(b"other").unwrap();
        assert_eq!(handle.read_verified().unwrap(), b"first");
        assert_ne!(handle.path(), file.path());
        assert_eq!(fs::read(handle.path()).unwrap(), b"first");
    }

    #[test]
    fn artifact_snapshot_survives_source_parent_rename_and_replacement() {
        let workspace = tempfile::tempdir().unwrap();
        let selected = workspace.path().join("selected");
        fs::create_dir(&selected).unwrap();
        let source = selected.join("plugin");
        fs::write(&source, b"admitted").unwrap();
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(b"admitted")));

        let handle = ArtifactHandle::open(&source, &digest, 8).unwrap();
        assert!(!handle.path().starts_with(&selected));
        fs::rename(&selected, workspace.path().join("replaced")).unwrap();
        fs::create_dir(&selected).unwrap();
        fs::write(selected.join("plugin"), b"attacker").unwrap();

        assert_eq!(fs::read(handle.path()).unwrap(), b"admitted");
        assert_eq!(handle.read_verified().unwrap(), b"admitted");
    }

    #[test]
    fn artifact_admission_streams_large_content_into_one_stable_snapshot() {
        let bytes = vec![0x5a; 4 * 1024 * 1024 + 17];
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&bytes).unwrap();
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));

        let handle = ArtifactHandle::open(file.path(), &digest, bytes.len() as u64).unwrap();

        assert_eq!(handle.read_verified().unwrap(), bytes);
    }

    /// Reproducible evidence command:
    /// `cargo test --release -p lenso-runtime-codec artifact_admission_streaming_benchmark -- --ignored --nocapture`
    #[test]
    #[ignore = "large Artifact admission benchmark; run explicitly"]
    fn artifact_admission_streaming_benchmark() {
        const BLOCK_BYTES: usize = 64 * 1024;
        let block = vec![0x5a; BLOCK_BYTES];
        for mebibytes in [4_usize, 64, 256] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("artifact");
            let mut source = fs::File::create(&path).unwrap();
            let mut hasher = Sha256::new();
            let blocks = mebibytes * 1024 * 1024 / BLOCK_BYTES;
            for _ in 0..blocks {
                source.write_all(&block).unwrap();
                hasher.update(&block);
            }
            drop(source);
            let size = u64::try_from(mebibytes * 1024 * 1024).unwrap();
            let digest = format!("sha256:{}", hex::encode(hasher.finalize()));

            let started = std::time::Instant::now();
            let handle = ArtifactHandle::open(&path, &digest, size).unwrap();
            let elapsed = started.elapsed();

            assert_eq!(handle.size(), size);
            println!(
                "{{\"mebibytes\":{mebibytes},\"elapsed_ms\":{:.3},\"mib_per_second\":{:.3}}}",
                elapsed.as_secs_f64() * 1_000.0,
                f64::from(u32::try_from(mebibytes).unwrap()) / elapsed.as_secs_f64()
            );
            drop(handle);
        }
    }

    #[test]
    fn artifact_admission_honors_an_explicit_host_staging_root() {
        let source = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        let path = source.path().join("plugin");
        fs::write(&path, b"artifact").unwrap();
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(b"artifact")));

        let handle =
            ArtifactHandle::open_with_staging_root(&path, &digest, 8, staging.path()).unwrap();

        assert_eq!(
            handle.path().parent().unwrap().parent().unwrap(),
            staging.path()
        );
    }

    #[test]
    fn instance_resources_are_order_independent_and_immutable() {
        let left = InstanceResources::from_files([
            ("prompts/system.md".to_owned(), b"Build carefully.".to_vec()),
            ("rules.toml".to_owned(), b"turns = 4\n".to_vec()),
        ])
        .unwrap();
        let right = InstanceResources::from_files([
            ("rules.toml".to_owned(), b"turns = 4\n".to_vec()),
            ("prompts/system.md".to_owned(), b"Build carefully.".to_vec()),
        ])
        .unwrap();

        assert_eq!(left.digest(), right.digest());
        assert_eq!(
            left.read_text("prompts/system.md").unwrap(),
            "Build carefully."
        );
        assert_eq!(left.file_count(), 2);
        assert_eq!(left.total_size(), 26);
    }

    #[test]
    fn instance_resources_reject_escaping_and_duplicate_paths() {
        assert!(InstanceResources::from_files([("../secret".to_owned(), Vec::new())]).is_err());
        assert!(
            InstanceResources::from_files([
                ("rules.toml".to_owned(), Vec::new()),
                ("rules.toml".to_owned(), Vec::new()),
            ])
            .is_err()
        );
    }
}
