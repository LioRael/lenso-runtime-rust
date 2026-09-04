//! Trusted, versioned C-ABI dynamic-library Execution Adapter.

mod abi;

use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    path::Path,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

use futures::{FutureExt, select};

use lenso_app_plan::{ExecutionClassId, PluginInstancePlan, ResolvedAppPlan};
use lenso_kernel::{
    ExecutionAdapter, InvocationContext, NativeRequestEndpoint, PluginLifecycle, PreparedNativeApp,
    PreparedNativePlugin, RuntimeFailure,
};
use lenso_runtime_codec::{
    ArtifactCatalog, ArtifactHandle, JsonCapabilityCodec, JsonInvocationOutcome,
    codecs_for_instance, prepare_request_app,
};

pub use abi::{
    ABI_VERSION, LensoBufferV1, LensoHostV1, LensoPluginV1, STATUS_DOMAIN_ERROR, STATUS_OK,
};

/// Stable open execution-class identity.
pub const EXECUTION_CLASS: &str = "lenso.native-dylib@1";
/// Exact runtime profile implemented by this Adapter release.
pub const RUNTIME_PROFILE: &str = "lenso.native-dylib@1";

/// Host-owned trust and platform-signature decision for exact dylib bytes.
pub trait DylibVerifier: std::fmt::Debug + 'static {
    /// Rejects any Artifact not explicitly admitted for native in-process execution.
    fn verify(&self, artifact: &ArtifactHandle) -> Result<(), RuntimeFailure>;
}

/// Exact-digest verifier for environments whose outer host already checked platform signing.
#[derive(Debug)]
pub struct ExplicitDigestTrust {
    digests: BTreeSet<String>,
}

impl ExplicitDigestTrust {
    /// Creates an explicit allow-list. Wildcard trust is intentionally unavailable.
    pub fn new(digests: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            digests: digests.into_iter().map(Into::into).collect(),
        }
    }
}

impl DylibVerifier for ExplicitDigestTrust {
    fn verify(&self, artifact: &ArtifactHandle) -> Result<(), RuntimeFailure> {
        if self.digests.contains(artifact.digest()) {
            Ok(())
        } else {
            Err(RuntimeFailure::InvalidResolvedPlan {
                detail: format!(
                    "native dylib `{}` lacks an exact explicit trust decision",
                    artifact.digest()
                ),
            })
        }
    }
}

/// Bounded native ABI inputs controlled by the Host Execution Policy.
#[derive(Clone, Debug)]
pub struct DylibLimits {
    pub max_request_bytes: usize,
    pub max_result_bytes: usize,
    pub max_descriptor_bytes: usize,
}

impl Default for DylibLimits {
    fn default() -> Self {
        Self {
            max_request_bytes: 1024 * 1024,
            max_result_bytes: 1024 * 1024,
            max_descriptor_bytes: 256 * 1024,
        }
    }
}

/// Experimental trusted native dylib Adapter.
#[derive(Debug)]
pub struct DylibAdapter {
    artifacts: ArtifactCatalog,
    verifier: Rc<dyn DylibVerifier>,
    codecs: BTreeMap<String, Rc<dyn JsonCapabilityCodec>>,
    limits: DylibLimits,
}

impl DylibAdapter {
    /// Creates an Adapter which requires one exact trust decision per Artifact.
    pub fn new(artifacts: ArtifactCatalog, verifier: impl DylibVerifier) -> Self {
        Self {
            artifacts,
            verifier: Rc::new(verifier),
            codecs: BTreeMap::new(),
            limits: DylibLimits::default(),
        }
    }

    /// Creates an Adapter from an already shared trust verifier.
    pub fn with_shared_verifier(
        artifacts: ArtifactCatalog,
        verifier: Rc<dyn DylibVerifier>,
    ) -> Self {
        Self {
            artifacts,
            verifier,
            codecs: BTreeMap::new(),
            limits: DylibLimits::default(),
        }
    }

    /// Registers one generated Capability codec.
    #[must_use]
    pub fn with_codec(mut self, codec: impl JsonCapabilityCodec) -> Self {
        self.codecs
            .insert(codec.capability_id().to_owned(), Rc::new(codec));
        self
    }

    /// Registers an already shared generated Capability codec.
    #[must_use]
    pub fn with_shared_codec(mut self, codec: Rc<dyn JsonCapabilityCodec>) -> Self {
        self.codecs.insert(codec.capability_id().to_owned(), codec);
        self
    }

    /// Applies host-policy byte limits.
    #[must_use]
    pub fn with_limits(mut self, limits: DylibLimits) -> Self {
        self.limits = limits;
        self
    }

    fn prepare_instance(
        &self,
        instance: &PluginInstancePlan,
    ) -> Result<PreparedNativePlugin, RuntimeFailure> {
        if instance.runtime_profile() != RUNTIME_PROFILE {
            return invalid(format!(
                "Dylib Adapter does not support runtime profile `{}`",
                instance.runtime_profile()
            ));
        }
        let artifact = self.artifacts.require(instance.instance_key())?;
        self.verifier.verify(artifact)?;
        validate_content_addressed_path(artifact)?;
        let codecs = codecs_for_instance(instance, &self.codecs)?;
        let capabilities = codecs
            .iter()
            .map(|codec| abi::CapabilityAbiDescriptor {
                capability_id: codec.capability_id(),
                descriptor_version: codec.descriptor_version(),
                request_operations: codec
                    .request_operations()
                    .iter()
                    .map(|operation| (*operation).to_owned())
                    .collect(),
            })
            .collect();
        let generation = Rc::new(DylibGeneration::start(
            artifact.clone(),
            capabilities,
            self.limits.clone(),
        )?);
        let endpoints = codecs
            .into_iter()
            .map(|codec| {
                Rc::new(DylibEndpoint {
                    generation: generation.clone(),
                    codec,
                }) as Rc<dyn NativeRequestEndpoint>
            })
            .collect();
        Ok(PreparedNativePlugin::new(
            endpoints,
            DylibLifecycle { generation },
        ))
    }
}

impl ExecutionAdapter for DylibAdapter {
    fn execution_class(&self) -> ExecutionClassId {
        ExecutionClassId::new(EXECUTION_CLASS)
    }

    fn prepare(&self, plan: &ResolvedAppPlan) -> Result<PreparedNativeApp, RuntimeFailure> {
        plan.validate()
            .map_err(|error| RuntimeFailure::InvalidResolvedPlan {
                detail: error.to_string(),
            })?;
        let execution_class = self.execution_class();
        let mut generations = BTreeMap::new();
        for instance in plan
            .plugin_instances()
            .iter()
            .filter(|instance| instance.execution_class() == &execution_class)
        {
            let generation = self.prepare_instance(instance)?;
            if generations
                .insert(instance.instance_key().to_owned(), generation)
                .is_some()
            {
                return invalid(format!("duplicate Instance `{}`", instance.instance_key()));
            }
        }
        prepare_request_app(plan, &execution_class, generations)
    }

    fn recreate(
        &self,
        plan: &ResolvedAppPlan,
        instance_key: &str,
    ) -> Result<PreparedNativePlugin, RuntimeFailure> {
        let instance = plan.plugin_instance(instance_key).ok_or_else(|| {
            RuntimeFailure::InvalidResolvedPlan {
                detail: format!("unknown Instance `{instance_key}`"),
            }
        })?;
        if instance.execution_class().as_str() != EXECUTION_CLASS {
            return invalid(format!("Instance `{instance_key}` is not a native dylib"));
        }
        self.prepare_instance(instance)
    }
}

struct DylibInvokeCommand {
    capability: String,
    operation: String,
    request: Vec<u8>,
    outcome: futures::channel::oneshot::Sender<Result<JsonInvocationOutcome, RuntimeFailure>>,
}

enum DylibWorkerCommand {
    Invoke(DylibInvokeCommand),
    Shutdown,
}

struct DylibGeneration {
    commands: mpsc::SyncSender<DylibWorkerCommand>,
    failed: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
    worker: RefCell<Option<thread::JoinHandle<()>>>,
    stopped: Cell<bool>,
}

impl std::fmt::Debug for DylibGeneration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DylibGeneration")
            .field("failed", &self.failed)
            .field("busy", &self.busy)
            .finish_non_exhaustive()
    }
}

impl DylibGeneration {
    fn start(
        artifact: ArtifactHandle,
        capabilities: Vec<abi::CapabilityAbiDescriptor>,
        limits: DylibLimits,
    ) -> Result<Self, RuntimeFailure> {
        let (commands, receiver) = mpsc::sync_channel(1);
        let (ready, ready_receiver) = mpsc::sync_channel(1);
        let failed = Arc::new(AtomicBool::new(false));
        let worker_failed = failed.clone();
        let busy = Arc::new(AtomicBool::new(false));
        let worker_busy = busy.clone();
        let worker = thread::Builder::new()
            .name("lenso-native-dylib".to_owned())
            .spawn(move || {
                let loaded = match abi::LoadedDylib::load(artifact, &capabilities, limits) {
                    Ok(loaded) => loaded,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                if ready.send(Ok(())).is_err() {
                    return;
                }
                while let Ok(command) = receiver.recv() {
                    match command {
                        DylibWorkerCommand::Shutdown => return,
                        DylibWorkerCommand::Invoke(command) => {
                            worker_busy.store(true, Ordering::Release);
                            let result = loaded.invoke(
                                &command.capability,
                                &command.operation,
                                &command.request,
                            );
                            worker_busy.store(false, Ordering::Release);
                            if result.is_err() {
                                worker_failed.store(true, Ordering::Release);
                            }
                            let _ = command.outcome.send(result);
                            if worker_failed.load(Ordering::Acquire) {
                                return;
                            }
                        }
                    }
                }
            })
            .map_err(|error| RuntimeFailure::Internal {
                detail: format!("failed to start native dylib worker: {error}"),
            })?;
        match ready_receiver.recv() {
            Ok(Ok(())) => Ok(Self {
                commands,
                failed,
                busy,
                worker: RefCell::new(Some(worker)),
                stopped: Cell::new(false),
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(error) => {
                let _ = worker.join();
                Err(RuntimeFailure::PluginFailure {
                    detail: format!("native dylib worker stopped before Ready: {error}"),
                })
            }
        }
    }

    async fn invoke(
        &self,
        capability: String,
        operation: String,
        request: Vec<u8>,
        context: InvocationContext,
    ) -> Result<JsonInvocationOutcome, RuntimeFailure> {
        if self.failed.load(Ordering::Acquire) {
            return Err(RuntimeFailure::PluginFailure {
                detail: "native dylib generation is retired".to_owned(),
            });
        }
        let (outcome, response) = futures::channel::oneshot::channel();
        self.commands
            .try_send(DylibWorkerCommand::Invoke(DylibInvokeCommand {
                capability,
                operation,
                request,
                outcome,
            }))
            .map_err(|_| RuntimeFailure::ResourceExhausted {
                capability: "lenso.native-dylib@1",
                operation: "invoke".to_owned(),
            })?;
        let cancellation = context.cancellation();
        let mut response = response.fuse();
        let mut cancelled = cancellation.cancelled().fuse();
        select! {
            result = response => match result {
                Ok(result) => result,
                Err(_) => Err(RuntimeFailure::PluginFailure {
                    detail: "native dylib worker stopped".to_owned(),
                }),
            },
            () = cancelled => {
                self.failed.store(true, Ordering::Release);
                Err(RuntimeFailure::Cancelled { request_id: context.request_id() })
            }
        }
    }

    fn stop(&self) {
        if self.stopped.replace(true) {
            return;
        }
        self.failed.store(true, Ordering::Release);
        let _ = self.commands.try_send(DylibWorkerCommand::Shutdown);
        if !self.busy.load(Ordering::Acquire)
            && let Some(worker) = self.worker.borrow_mut().take()
        {
            let _ = worker.join();
        } else {
            // A trusted native call cannot be preempted safely. Detaching a stuck worker is the
            // explicit experimental failure policy; reclaiming it requires Host restart, just as
            // reclaiming the deliberately non-unloaded library mapping does.
            let _ = self.worker.borrow_mut().take();
        }
    }
}

impl Drop for DylibGeneration {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug)]
struct DylibEndpoint {
    generation: Rc<DylibGeneration>,
    codec: Rc<dyn JsonCapabilityCodec>,
}

impl NativeRequestEndpoint for DylibEndpoint {
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
        let generation = self.generation.clone();
        let codec = self.codec.clone();
        let operation = operation.to_owned();
        Box::pin(async move {
            if context.cancellation().is_cancelled() {
                return Err(RuntimeFailure::Cancelled {
                    request_id: context.request_id(),
                });
            }
            if !codec.request_operations().contains(&operation.as_str()) {
                return Err(RuntimeFailure::UnknownOperation {
                    capability: codec.capability_id(),
                    operation,
                });
            }
            let request = codec.encode_request(&operation, request.as_ref())?;
            let request =
                serde_json::to_vec(&request).map_err(|_| RuntimeFailure::ProtocolViolation {
                    capability: codec.capability_id(),
                })?;
            let outcome = generation
                .invoke(
                    codec.capability_id().to_owned(),
                    operation.clone(),
                    request,
                    context,
                )
                .await?;
            match outcome {
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

#[derive(Debug)]
struct DylibLifecycle {
    generation: Rc<DylibGeneration>,
}

impl PluginLifecycle for DylibLifecycle {
    fn deactivate(&self, _context: lenso_kernel::DeactivateContext) -> lenso_kernel::PluginFuture {
        self.generation.stop();
        Box::pin(futures::future::ready(Ok(())))
    }
}

fn validate_content_addressed_path(artifact: &ArtifactHandle) -> Result<(), RuntimeFailure> {
    let expected = artifact
        .digest()
        .strip_prefix("sha256:")
        .expect("ArtifactHandle validates canonical digest syntax");
    if artifact.path().file_name().and_then(|name| name.to_str()) != Some(expected) {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: format!(
                "native dylib `{}` is not loaded from its content-addressed path",
                artifact.path().display()
            ),
        });
    }
    if !is_native_library(artifact.path()) {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "native dylib Artifact does not use a supported platform file type".to_owned(),
        });
    }
    Ok(())
}

fn is_native_library(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_none()
        || matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("dylib" | "so" | "dll")
        )
}

fn invalid<T>(detail: String) -> Result<T, RuntimeFailure> {
    Err(RuntimeFailure::InvalidResolvedPlan { detail })
}

#[cfg(target_os = "macos")]
pub mod macos {
    //! macOS platform-signature verification for native dylib admission.

    use std::process::Command;

    use super::{ArtifactHandle, BTreeSet, DylibVerifier, RuntimeFailure};

    /// Requires a valid strict code signature and one exact Team Identifier.
    #[derive(Debug)]
    pub struct CodeSignatureVerifier {
        team_identifier: String,
        digests: BTreeSet<String>,
    }

    impl CodeSignatureVerifier {
        pub fn new(
            team_identifier: impl Into<String>,
            digests: impl IntoIterator<Item = impl Into<String>>,
        ) -> Self {
            Self {
                team_identifier: team_identifier.into(),
                digests: digests.into_iter().map(Into::into).collect(),
            }
        }
    }

    impl DylibVerifier for CodeSignatureVerifier {
        fn verify(&self, artifact: &ArtifactHandle) -> Result<(), RuntimeFailure> {
            if !self.digests.contains(artifact.digest()) {
                return Err(RuntimeFailure::InvalidResolvedPlan {
                    detail: "native dylib digest lacks explicit trust".to_owned(),
                });
            }
            let verification = Command::new("/usr/bin/codesign")
                .args(["--verify", "--strict", "--verbose=2"])
                .arg(artifact.path())
                .output()
                .map_err(|error| RuntimeFailure::Internal {
                    detail: format!("failed to execute codesign: {error}"),
                })?;
            if !verification.status.success() {
                return Err(RuntimeFailure::InvalidResolvedPlan {
                    detail: "native dylib has an invalid code signature".to_owned(),
                });
            }
            let details = Command::new("/usr/bin/codesign")
                .args(["-dv", "--verbose=4"])
                .arg(artifact.path())
                .output()
                .map_err(|error| RuntimeFailure::Internal {
                    detail: format!("failed to inspect codesign identity: {error}"),
                })?;
            let stderr = String::from_utf8_lossy(&details.stderr);
            let expected = format!("TeamIdentifier={}", self.team_identifier);
            if !details.status.success() || !stderr.lines().any(|line| line == expected) {
                return Err(RuntimeFailure::InvalidResolvedPlan {
                    detail: "native dylib Team Identifier is not admitted".to_owned(),
                });
            }
            Ok(())
        }
    }
}
