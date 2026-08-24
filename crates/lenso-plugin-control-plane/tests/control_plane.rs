use std::collections::BTreeMap;

use lenso_app_plan::ResolvedAppPlan;
use lenso_kernel::ExecutionAdapterCatalog;
use lenso_plugin_control_plane::*;
use lenso_runtime_codec::ArtifactCatalog;
use sha2::{Digest, Sha256};

#[derive(Debug)]
struct AllowExact;

impl AdmissionPolicy for AllowExact {
    fn admit(
        &self,
        _manifest: &PluginManifest,
        _manifest_digest: &str,
        _artifact_digests: &[String],
        _product_metadata_digests: &[String],
        provenance: &str,
    ) -> Result<String, ControlPlaneError> {
        if provenance == "local-review" {
            Ok("operator-approved-test-release".to_owned())
        } else {
            Err(ControlPlaneError::AdmissionRejected {
                detail: "unexpected provenance".to_owned(),
            })
        }
    }

    fn identity(&self) -> &'static str {
        "test.allow-exact@1"
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn store_policy_resolution_and_generation_authority_close() {
    let source = b"export function invoke(operation, requestJson) { return JSON.stringify({ok: JSON.parse(requestJson)}); }";
    let unused_source = b"export function invoke() { return JSON.stringify({unused: true}); }";
    let artifact_digest = digest(source);
    let manifest = PluginManifest {
        schema_version: 1,
        plugin_id: "example.echo".to_owned(),
        release_version: "1.0.0".to_owned(),
        artifacts: vec![
            ArtifactDeclaration {
                id: "quickjs".to_owned(),
                kind: ArtifactKind::QuickJsModule,
                digest: artifact_digest.clone(),
                size: source.len() as u64,
                media_type: "text/javascript".to_owned(),
                path: "plugin.mjs".to_owned(),
                targets: vec!["aarch64-apple-darwin".to_owned()],
            },
            ArtifactDeclaration {
                id: "unused-candidate".to_owned(),
                kind: ArtifactKind::QuickJsModule,
                digest: digest(unused_source),
                size: unused_source.len() as u64,
                media_type: "text/javascript".to_owned(),
                path: "unused.mjs".to_owned(),
                targets: vec!["aarch64-apple-darwin".to_owned()],
            },
        ],
        module_contributions: vec![ModuleContribution {
            id: "echo".to_owned(),
            package_id: "example.echo".to_owned(),
            configuration_schema_digest: digest(b"configuration"),
            provides: vec![CapabilityDeclaration {
                capability_id: "example.echo@1".to_owned(),
                descriptor_version: "1.0.0".to_owned(),
                descriptor_digest: digest(b"descriptor"),
                request_operations: vec!["echo".to_owned()],
            }],
            requires: Vec::new(),
            implementations: vec![ImplementationVariant {
                id: "quickjs-aarch64".to_owned(),
                artifact: Some("quickjs".to_owned()),
                built_in_factory: None,
                entrypoint: "plugin.mjs".to_owned(),
                execution_class: "lenso.quickjs@1".to_owned(),
                targets: vec!["aarch64-apple-darwin".to_owned()],
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
    };
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    let mut files = BTreeMap::new();
    files.insert("plugin.mjs".to_owned(), source.to_vec());
    files.insert("unused.mjs".to_owned(), unused_source.to_vec());
    let store_directory = tempfile::tempdir().unwrap();
    let store = PluginStore::open(store_directory.path()).unwrap();
    let receipt = store
        .admit(
            &PluginBundle::new(manifest_bytes.clone(), files, "local-review"),
            &AllowExact,
        )
        .unwrap();
    let manifest = CanonicalDocument::<PluginManifest>::parse("manifest", &manifest_bytes).unwrap();
    let lock = CanonicalDocument::from_value(
        "lock",
        PluginSetLock {
            schema_version: 1,
            app_id: "example-app".to_owned(),
            plugins: vec![LockedPlugin {
                plugin_id: "example.echo".to_owned(),
                release_version: "1.0.0".to_owned(),
                manifest_digest: manifest.digest().to_owned(),
                selected_features: Vec::new(),
                product_metadata_digests: Vec::new(),
            }],
            instances: vec![LockedInstance {
                plugin_id: "example.echo".to_owned(),
                contribution_id: "echo".to_owned(),
                instance_key: "echo-plugin".to_owned(),
                implementation_variant: None,
                configuration: "{}".to_owned(),
                execution_lane: "main".to_owned(),
            }],
            data_mounts: Vec::new(),
            approved_grants: Vec::new(),
        },
    )
    .unwrap();
    let host_build = CanonicalDocument::from_value(
        "host build",
        HostBuildManifest {
            schema_version: 1,
            app_id: "example-app".to_owned(),
            host_executable_digest: digest(b"host"),
            target: "aarch64-apple-darwin".to_owned(),
            built_in_modules: Vec::new(),
            adapter_profiles: vec![AdapterProfile {
                execution_class: "lenso.quickjs@1".to_owned(),
                adapter_build_identity: "lenso-quickjs-adapter@0.1.0".to_owned(),
                targets: vec!["aarch64-apple-darwin".to_owned()],
                profiles: vec!["provide-request-v1".to_owned()],
            }],
        },
    )
    .unwrap();
    let policy = CanonicalDocument::from_value(
        "execution policy",
        HostExecutionPolicy {
            schema_version: 1,
            app_id: "example-app".to_owned(),
            host_build_manifest_digest: host_build.digest().to_owned(),
            target: "aarch64-apple-darwin".to_owned(),
            classes: vec![ClassPolicy {
                execution_class: "lenso.quickjs@1".to_owned(),
                support_channels: vec![SupportChannel::Preview],
                trust_levels: vec![TrustLevel::Constrained],
                profiles: vec!["provide-request-v1".to_owned()],
            }],
            preference: vec!["lenso.quickjs@1".to_owned()],
            instance_overrides: Vec::new(),
        },
    )
    .unwrap();
    let mut manifests = BTreeMap::new();
    manifests.insert("example.echo".to_owned(), manifest);
    let mut receipts = BTreeMap::new();
    receipts.insert(
        lock.value().plugins[0].manifest_digest.clone(),
        receipt.digest().to_owned(),
    );
    let resolved = resolve_generation(&ResolutionInput {
        lock: &lock,
        manifests: &manifests,
        admission_receipts: &receipts,
        host_build: &host_build,
        policy: &policy,
        store: &store,
        base_instances: Vec::new(),
        bindings: Vec::new(),
    })
    .unwrap();

    assert_eq!(resolved.plan.module_instances().len(), 1);
    assert_eq!(
        resolved.plan.module_instances()[0]
            .execution_class()
            .as_str(),
        "lenso.quickjs@1"
    );
    assert_eq!(resolved.artifact_set.value().instances.len(), 1);
    assert_eq!(resolved.spec.value().plugin_set_lock_digest, lock.digest());
    assert!(resolved.artifacts.require("echo-plugin").is_ok());
}

#[derive(Debug, Default)]
struct FakeRuntime {
    stopped: Vec<String>,
}

impl GenerationRuntime for FakeRuntime {
    type Handle = String;

    fn stage<'a>(
        &'a mut self,
        generation: &'a ResolvedGeneration,
        _ready_timeout_nanos: u64,
    ) -> futures::future::LocalBoxFuture<'a, Result<Self::Handle, ControlPlaneError>> {
        Box::pin(async move { Ok(generation.spec.digest().to_owned()) })
    }

    fn shutdown(
        &mut self,
        handle: Self::Handle,
        _drain_timeout_nanos: u64,
    ) -> futures::future::LocalBoxFuture<'_, Result<(), ControlPlaneError>> {
        Box::pin(async move {
            self.stopped.push(handle);
            Ok(())
        })
    }
}

#[test]
fn supervisor_fences_switches_pins_and_retires_generations() {
    futures::executor::block_on(async {
        let first = empty_generation("first");
        let second = empty_generation("second");
        let initial = transition(None, &first, ReplacementMode::Initial, "0");
        let mut supervisor = GenerationSupervisor::new("app", FakeRuntime::default());
        let first_outcome = supervisor.transition(&initial, &first).await.unwrap();
        assert_eq!(first_outcome.routing_epoch, 1);
        let lease = supervisor.lease().unwrap();
        assert_eq!(lease.generation_spec_digest(), first.spec.digest());

        let overlap = transition(
            Some(first.spec.digest()),
            &second,
            ReplacementMode::Overlap,
            "100",
        );
        let second_outcome = supervisor.transition(&overlap, &second).await.unwrap();
        assert_eq!(second_outcome.routing_epoch, 2);
        assert_eq!(supervisor.generations().len(), 2);
        drop(lease);
        supervisor
            .close_rollback_window(first.spec.digest())
            .unwrap();
        supervisor.retire(first.spec.digest(), 100).await.unwrap();
        assert_eq!(supervisor.generations().len(), 1);
    });
}

#[test]
fn overlap_requires_exact_state_compatibility_receipt_closure() {
    futures::executor::block_on(async {
        let mut first = empty_generation("stateful-first");
        first.stateful_instances.insert(
            "stateful".to_owned(),
            StatefulRuntimeIdentity {
                runtime_identity: "plugin:manifest-v1:artifact-v1".to_owned(),
                state_schema_id: "example.state@1".to_owned(),
                state_schema_digest: digest(b"state-v1"),
            },
        );
        let mut second = empty_generation("stateful-second");
        second.stateful_instances.insert(
            "stateful".to_owned(),
            StatefulRuntimeIdentity {
                runtime_identity: "plugin:manifest-v2:artifact-v2".to_owned(),
                state_schema_id: "example.state@1".to_owned(),
                state_schema_digest: digest(b"state-v2"),
            },
        );
        let receipt = CanonicalDocument::from_value(
            "state compatibility receipt",
            StateCompatibilityReceipt {
                schema_version: 1,
                app_id: "app".to_owned(),
                module_instance_key: "stateful".to_owned(),
                old_runtime_identity: "plugin:manifest-v1:artifact-v1".to_owned(),
                new_runtime_identity: "plugin:manifest-v2:artifact-v2".to_owned(),
                state_schema_id: "example.state@1".to_owned(),
                old_state_schema_digest: digest(b"state-v1"),
                new_state_schema_digest: digest(b"state-v2"),
                compatibility: StateCompatibility {
                    concurrent_read: true,
                    concurrent_write: true,
                    old_code_reads_new_writes: true,
                },
                policy_digest: digest(b"state-policy"),
                evidence_digest: digest(b"state-evidence"),
                decision_authority: "product.release-review".to_owned(),
            },
        )
        .unwrap();
        let overlap = transition_with_receipts(
            Some(first.spec.digest()),
            &second,
            ReplacementMode::Overlap,
            "100",
            true,
            vec![receipt.digest().to_owned()],
        );
        let mut supervisor = GenerationSupervisor::new("app", FakeRuntime::default());
        supervisor
            .transition(
                &transition(None, &first, ReplacementMode::Initial, "0"),
                &first,
            )
            .await
            .unwrap();
        assert!(supervisor.transition(&overlap, &second).await.is_err());

        let receipts = BTreeMap::from([(receipt.digest().to_owned(), receipt)]);
        supervisor
            .transition_with_receipts(&overlap, &second, &receipts)
            .await
            .unwrap();
    });
}

#[test]
fn durable_supervisor_recovers_fences_drains_and_rolls_back() {
    futures::executor::block_on(async {
        let directory = tempfile::tempdir().unwrap();
        let store = FileControlStateStore::open(directory.path()).unwrap();
        let first = empty_generation("durable-first");
        let second = empty_generation("durable-second");
        let mut supervisor =
            DurableGenerationSupervisor::open("app", FakeRuntime::default(), store.clone())
                .unwrap();
        supervisor
            .transition(
                &transition(None, &first, ReplacementMode::Initial, "0"),
                &first,
                &BTreeMap::new(),
                0,
            )
            .await
            .unwrap();
        let first_lease = supervisor.lease().unwrap();
        let old_epoch = first_lease.routing_epoch();
        let overlap = transition_with_receipts(
            Some(first.spec.digest()),
            &second,
            ReplacementMode::Overlap,
            "100",
            true,
            Vec::new(),
        );
        let outcome = supervisor
            .transition(&overlap, &second, &BTreeMap::new(), 100)
            .await
            .unwrap();
        assert!(supervisor.lease_at_epoch(old_epoch).is_err());
        assert_eq!(outcome.routing_epoch, old_epoch + 1);
        drop(first_lease);
        supervisor
            .complete_drain(first.spec.digest(), 150)
            .await
            .unwrap();
        assert_eq!(
            supervisor
                .state()
                .generations
                .iter()
                .find(|record| record.generation_spec_digest == first.spec.digest())
                .unwrap()
                .lifecycle,
            ControlLifecycle::Standby
        );

        let rollback = supervisor
            .mark_generation_failed(second.spec.digest(), 160)
            .unwrap()
            .expect("automatic rollback should activate exact standby");
        assert_eq!(rollback.activation_direction, ActivationDirection::Rollback);
        assert_eq!(rollback.active_generation_spec_digest, first.spec.digest());
        supervisor
            .complete_drain(second.spec.digest(), 160)
            .await
            .unwrap();
        let before_recovery = supervisor.state().clone();
        drop(supervisor);

        let authorities = BTreeMap::from([(first.spec.digest().to_owned(), first.clone())]);
        let recovered = DurableGenerationSupervisor::recover(
            "app",
            FakeRuntime::default(),
            store,
            &authorities,
            170,
        )
        .await
        .unwrap();
        assert_eq!(
            recovered.state().active_generation_spec_digest.as_deref(),
            Some(first.spec.digest())
        );
        assert!(recovered.state().supervisor_epoch > before_recovery.supervisor_epoch);
        assert!(recovered.state().routing_epoch > before_recovery.routing_epoch);
        assert!(recovered.lease().is_ok());
    });
}

#[derive(Debug)]
struct EmptyCatalog;

impl CatalogFactory for EmptyCatalog {
    fn catalog(
        &self,
        _generation: &ResolvedGeneration,
    ) -> Result<ExecutionAdapterCatalog, ControlPlaneError> {
        Ok(ExecutionAdapterCatalog::new())
    }
}

#[test]
fn kernel_generation_runtime_starts_ready_and_shuts_down_real_kernel() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async {
        let generation = empty_generation("kernel");
        let mut host = KernelGenerationRuntime::new(EmptyCatalog);
        let handle = host.stage(&generation, 1_000_000_000).await.unwrap();
        host.shutdown(handle, 1_000_000_000).await.unwrap();
    });
}

fn empty_generation(marker: &str) -> ResolvedGeneration {
    let plan = ResolvedAppPlan::empty();
    let artifact_set = CanonicalDocument::from_value(
        "artifact set",
        ResolvedArtifactSet {
            schema_version: 1,
            plugin_set_lock_digest: digest(b"lock"),
            host_execution_policy_digest: digest(b"policy"),
            releases: Vec::new(),
            artifacts: Vec::new(),
            instances: Vec::new(),
            data_mounts: Vec::new(),
        },
    )
    .unwrap();
    let grants = CanonicalDocument::from_value(
        "grants",
        EffectiveHostGrantSet {
            schema_version: 1,
            plugin_set_lock_digest: digest(b"lock"),
            grants: Vec::new(),
        },
    )
    .unwrap();
    let spec = CanonicalDocument::from_value(
        "generation",
        AppGenerationSpec {
            schema_version: 1,
            app_id: "app".to_owned(),
            host_build_manifest_digest: digest(b"host"),
            host_execution_policy_digest: digest(b"policy"),
            resolved_plan_digest: digest(marker.as_bytes()),
            plugin_set_lock_digest: digest(b"lock"),
            resolved_artifact_set_digest: artifact_set.digest().to_owned(),
            effective_host_grant_set_digest: grants.digest().to_owned(),
        },
    )
    .unwrap();
    ResolvedGeneration {
        plan,
        artifact_set,
        grants,
        spec,
        artifacts: ArtifactCatalog::new(),
        stateful_instances: BTreeMap::new(),
    }
}

fn transition(
    from: Option<&str>,
    to: &ResolvedGeneration,
    replacement_mode: ReplacementMode,
    rollback_window_nanos: &str,
) -> CanonicalDocument<AppGenerationTransitionSpec> {
    transition_with_receipts(
        from,
        to,
        replacement_mode,
        rollback_window_nanos,
        false,
        Vec::new(),
    )
}

fn transition_with_receipts(
    from: Option<&str>,
    to: &ResolvedGeneration,
    replacement_mode: ReplacementMode,
    rollback_window_nanos: &str,
    automatic_rollback_on_generation_failure: bool,
    state_compatibility_receipt_digests: Vec<String>,
) -> CanonicalDocument<AppGenerationTransitionSpec> {
    CanonicalDocument::from_value(
        "transition",
        AppGenerationTransitionSpec {
            schema_version: 1,
            app_id: "app".to_owned(),
            from_generation_spec_digest: from.map(str::to_owned),
            to_generation_spec_digest: to.spec.digest().to_owned(),
            replacement_mode,
            state_compatibility_receipt_digests,
            rollout_policy: RolloutPolicy {
                ready_timeout_nanos: "1000000000".to_owned(),
                drain_timeout_nanos: "1000000000".to_owned(),
                rollback_window_nanos: rollback_window_nanos.to_owned(),
                automatic_rollback_on_generation_failure,
            },
        },
    )
    .unwrap()
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}
