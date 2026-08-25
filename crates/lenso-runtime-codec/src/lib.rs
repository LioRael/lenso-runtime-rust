//! Shared artifact and generated Capability codec seams for Execution Adapters.

use std::{
    any::Any,
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
};

use lenso_app_plan::{ExecutionClassId, ModuleInstancePlan, ResolvedAppPlan};
use lenso_kernel::{
    InvocationContext, NativeRequestEndpoint, NativeStreamEndpoint, NativeStreamItem,
    NativeStreamSession, PreparedBinding, PreparedNativeApp, PreparedNativeModule,
    PreparedStreamBinding, RuntimeFailure,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Digest-verified, read-only execution input selected before Adapter preparation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactHandle {
    path: PathBuf,
    digest: String,
    size: u64,
}

impl ArtifactHandle {
    /// Verifies one regular file against its canonical SHA-256 digest and size.
    pub fn open(
        path: impl Into<PathBuf>,
        expected_digest: &str,
        expected_size: u64,
    ) -> Result<Self, RuntimeFailure> {
        validate_digest(expected_digest)?;
        let path = path.into();
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
        let bytes = fs::read(&path).map_err(|error| invalid_artifact(&path, error))?;
        let actual_digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
        if actual_digest != expected_digest {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: format!("Artifact `{}` digest mismatch", path.display()),
            });
        }
        Ok(Self {
            path,
            digest: actual_digest,
            size: metadata.len(),
        })
    }

    /// Returns the verified machine-local path. It is never serialized into a Plan.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the verified content identity.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Returns the verified byte size.
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// Reads the bytes again and fails if they changed since admission.
    pub fn read_verified(&self) -> Result<Vec<u8>, RuntimeFailure> {
        let verified = Self::open(&self.path, &self.digest, self.size)?;
        fs::read(verified.path).map_err(|error| invalid_artifact(&self.path, error))
    }
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
}

/// Exact host outcome returned by a byte-oriented Module invocation.
#[derive(Debug)]
pub enum JsonInvocationOutcome {
    /// Successful generated response value.
    Success(Value),
    /// Declared generated Domain Error value.
    DomainError(Value),
}

/// Stable request-only guest ABI implemented by byte-oriented Module runtimes.
pub const JSON_REQUEST_ABI_V1: &str = "lenso.json-request@1";

/// Stable Request and bidirectional Stream guest ABI.
pub const JSON_INTERACTIONS_ABI_V1: &str = "lenso.json-interactions@1";

/// Exact guest declaration returned before an Adapter opens readiness.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JsonModuleDescriptor {
    pub abi: String,
    pub capabilities: Vec<JsonCapabilityDescriptor>,
}

/// One exact request Capability exposed by a guest Module.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JsonCapabilityDescriptor {
    pub capability_id: String,
    pub descriptor_version: String,
    pub request_operations: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stream_operations: Vec<String>,
}

/// Derives the only guest declaration accepted for one resolved Instance.
pub fn expected_json_module_descriptor(
    instance: &ModuleInstancePlan,
) -> Result<JsonModuleDescriptor, RuntimeFailure> {
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
    Ok(JsonModuleDescriptor {
        abi: if capabilities
            .iter()
            .any(|capability| !capability.stream_operations.is_empty())
        {
            JSON_INTERACTIONS_ABI_V1
        } else {
            JSON_REQUEST_ABI_V1
        }
        .to_owned(),
        capabilities,
    })
}

/// Parses and compares a guest Ready declaration with exact Plan authority.
pub fn validate_json_module_descriptor(
    instance: &ModuleInstancePlan,
    encoded: &str,
) -> Result<(), RuntimeFailure> {
    let mut actual = serde_json::from_str::<JsonModuleDescriptor>(encoded).map_err(|_| {
        RuntimeFailure::ProtocolViolation {
            capability: "lenso.json-request@1",
        }
    })?;
    actual.capabilities.sort();
    let expected = expected_json_module_descriptor(instance)?;
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
    instance: &ModuleInstancePlan,
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

/// Builds exact request bindings from Adapter-prepared Module generations.
pub fn prepare_request_app(
    plan: &ResolvedAppPlan,
    execution_class: &ExecutionClassId,
    generations: BTreeMap<String, PreparedNativeModule>,
) -> Result<PreparedNativeApp, RuntimeFailure> {
    let selected_instances = plan
        .module_instances()
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
        .module_instances()
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
            bindings.push(PreparedBinding::new(
                binding.consumer_instance(),
                binding.provider_instance(),
                endpoint.clone(),
            ));
        }
        if let Some(endpoint) = stream_endpoint {
            stream_bindings.push(PreparedStreamBinding::new(
                binding.consumer_instance(),
                binding.provider_instance(),
                endpoint.clone(),
            ));
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

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn artifact_handle_rejects_digest_drift() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"first").unwrap();
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(b"first")));
        let handle = ArtifactHandle::open(file.path(), &digest, 5).unwrap();
        file.as_file_mut().set_len(0).unwrap();
        file.write_all(b"other").unwrap();
        assert!(handle.read_verified().is_err());
    }
}
