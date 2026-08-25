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
    InvocationContext, NativeRequestEndpoint, PreparedBinding, PreparedNativeApp,
    PreparedNativeModule, RuntimeFailure,
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
}

/// Derives the only guest declaration accepted for one resolved Instance.
pub fn expected_json_module_descriptor(
    instance: &ModuleInstancePlan,
) -> Result<JsonModuleDescriptor, RuntimeFailure> {
    let mut capabilities = Vec::with_capacity(instance.provided_capabilities().len());
    for descriptor in instance.provided_capabilities() {
        if !descriptor.stream_operations().is_empty() || !descriptor.event_operations().is_empty() {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: format!(
                    "Execution class `{}` supports Request endpoints only",
                    instance.execution_class()
                ),
            });
        }
        capabilities.push(JsonCapabilityDescriptor {
            capability_id: descriptor.capability_id().to_owned(),
            descriptor_version: descriptor.descriptor_version().to_owned(),
            request_operations: descriptor.operations().to_vec(),
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
        abi: JSON_REQUEST_ABI_V1.to_owned(),
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

/// Builds typed Kernel endpoints over one exact guest transport generation.
pub fn json_request_endpoints<T: JsonRequestTransport>(
    transport: Rc<T>,
    codecs: Vec<Rc<dyn JsonCapabilityCodec>>,
) -> Vec<Rc<dyn NativeRequestEndpoint>> {
    let transport: Rc<dyn JsonRequestTransport> = transport;
    codecs
        .into_iter()
        .map(|codec| {
            Rc::new(JsonRequestEndpoint {
                transport: transport.clone(),
                codec,
            }) as Rc<dyn NativeRequestEndpoint>
        })
        .collect()
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
        if !descriptor.stream_operations().is_empty() || !descriptor.event_operations().is_empty() {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: format!(
                    "Execution class `{}` supports Request endpoints only",
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
        let operations: Vec<_> = codec
            .request_operations()
            .iter()
            .map(|operation| (*operation).to_owned())
            .collect();
        if codec.descriptor_version() != descriptor.descriptor_version()
            || operations != descriptor.operations()
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
    for (instance_key, generation) in &generations {
        for endpoint in generation.endpoints() {
            let identity = (instance_key.clone(), endpoint.capability_id().to_owned());
            if endpoints.insert(identity, endpoint.clone()).is_some() {
                return Err(RuntimeFailure::InvalidResolvedPlan {
                    detail: format!("duplicate request endpoint on Instance `{instance_key}`"),
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
    for binding in plan.capability_bindings() {
        let key = (
            binding.provider_instance().to_owned(),
            binding.capability_id().to_owned(),
        );
        if let Some(endpoint) = endpoints.get(&key) {
            bindings.push(PreparedBinding::new(
                binding.consumer_instance(),
                binding.provider_instance(),
                endpoint.clone(),
            ));
        } else if selected_instances.contains(binding.provider_instance()) {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: format!(
                    "Adapter omitted Capability `{}` endpoint for Instance `{}`",
                    binding.capability_id(),
                    binding.provider_instance()
                ),
            });
        }
    }
    Ok(PreparedNativeApp::new(bindings, generations))
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
