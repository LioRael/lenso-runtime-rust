use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use lenso_app_plan::{
    AppComposition, CapabilityOperationKind, ExecutionLaneId, ExecutionLanePlan, ModuleCriticality,
    ModuleInstancePlan, ResolvedAppPlan, RestartPolicy,
};
use lenso_kernel::{
    ActivateContext, ExecutionAdapterCatalog, ModuleFuture, ModuleLifecycle,
    NativeExecutionAdapter, RuntimeFailure,
};
use lenso_native_adapter::{
    NativeModuleFactory, NativeModuleFactoryContext, NativeModuleInstance, NativeModuleRegistry,
};
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

#[derive(Debug)]
struct BuiltInPluginFactory;

impl NativeModuleFactory for BuiltInPluginFactory {
    fn package_id(&self) -> &'static str {
        "example.builtin"
    }

    fn package_version(&self) -> &'static str {
        "1.0.0"
    }

    fn factory_identity(&self) -> String {
        "example.builtin@host-build-a".to_owned()
    }

    fn instantiate(
        &self,
        _context: NativeModuleFactoryContext<'_>,
    ) -> Result<NativeModuleInstance, RuntimeFailure> {
        Ok(NativeModuleInstance::default())
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
                operation_kinds: BTreeMap::from([(
                    "echo".to_owned(),
                    CapabilityOperationKind::Stream,
                )]),
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
    assert_eq!(
        resolved.plan.module_instances()[0].provided_capabilities()[0].stream_operations(),
        ["echo"]
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn built_in_plugin_factory_identity_closes_into_native_preparation() {
    let factory = BuiltInPluginFactory;
    let factory_identity = factory.factory_identity();
    let manifest = PluginManifest {
        schema_version: 1,
        plugin_id: "example.builtin-plugin".to_owned(),
        release_version: "1.0.0".to_owned(),
        artifacts: Vec::new(),
        module_contributions: vec![ModuleContribution {
            id: "builtin".to_owned(),
            package_id: factory.package_id().to_owned(),
            configuration_schema_digest: digest(b"configuration"),
            provides: Vec::new(),
            requires: Vec::new(),
            implementations: vec![ImplementationVariant {
                id: "native".to_owned(),
                artifact: None,
                built_in_factory: Some(factory_identity.clone()),
                entrypoint: "default".to_owned(),
                execution_class: "lenso.native-rust@1".to_owned(),
                targets: vec!["aarch64-apple-darwin".to_owned()],
                profiles: vec!["native-v1".to_owned()],
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
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    let store_directory = tempfile::tempdir().unwrap();
    let store = PluginStore::open(store_directory.path()).unwrap();
    let receipt = store
        .admit(
            &PluginBundle::new(manifest_bytes.clone(), BTreeMap::new(), "local-review"),
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
                plugin_id: "example.builtin-plugin".to_owned(),
                release_version: "1.0.0".to_owned(),
                manifest_digest: manifest.digest().to_owned(),
                selected_features: Vec::new(),
                product_metadata_digests: Vec::new(),
            }],
            instances: vec![LockedInstance {
                plugin_id: "example.builtin-plugin".to_owned(),
                contribution_id: "builtin".to_owned(),
                instance_key: "builtin-plugin".to_owned(),
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
            built_in_modules: vec![BuiltInModule {
                package_id: factory.package_id().to_owned(),
                factory_identity: factory_identity.clone(),
                execution_class: "lenso.native-rust@1".to_owned(),
            }],
            adapter_profiles: vec![AdapterProfile {
                execution_class: "lenso.native-rust@1".to_owned(),
                adapter_build_identity: "lenso-native-adapter@0.1.2".to_owned(),
                targets: vec!["aarch64-apple-darwin".to_owned()],
                profiles: vec!["native-v1".to_owned()],
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
                execution_class: "lenso.native-rust@1".to_owned(),
                support_channels: vec![SupportChannel::Stable],
                trust_levels: vec![TrustLevel::Trusted],
                profiles: vec!["native-v1".to_owned()],
            }],
            preference: vec!["lenso.native-rust@1".to_owned()],
            instance_overrides: Vec::new(),
        },
    )
    .unwrap();
    let mut manifests = BTreeMap::new();
    manifests.insert("example.builtin-plugin".to_owned(), manifest);
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

    assert_eq!(
        resolved.plan.module_instances()[0].package_revision(),
        factory_identity
    );
    NativeModuleRegistry::new()
        .with_factory(factory)
        .prepare(&resolved.plan)
        .expect("the exact built-in factory identity should prepare natively");
}

#[derive(Debug, Default)]
struct FakeRuntime {
    stopped: Vec<String>,
    failures: Rc<RefCell<BTreeMap<String, ControlPlaneError>>>,
    stage_failures: Rc<RefCell<BTreeSet<String>>>,
}

impl GenerationRuntime for FakeRuntime {
    type Handle = String;
    type Route = String;

    fn stage<'a>(
        &'a mut self,
        generation: &'a ResolvedGeneration,
        _ready_timeout_nanos: u64,
    ) -> futures::future::LocalBoxFuture<'a, Result<Self::Handle, ControlPlaneError>> {
        Box::pin(async move {
            if self
                .stage_failures
                .borrow()
                .contains(generation.spec.digest())
            {
                return Err(ControlPlaneError::HostFailure {
                    detail: "configured staging failure".to_owned(),
                });
            }
            Ok(generation.spec.digest().to_owned())
        })
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

    fn terminal_failure(&self, handle: &Self::Handle) -> Option<ControlPlaneError> {
        self.failures.borrow().get(handle).cloned()
    }

    fn route(&self, handle: &Self::Handle) -> Self::Route {
        handle.clone()
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
#[allow(clippy::too_many_lines)]
fn durable_supervisor_recovers_fences_drains_and_rolls_back() {
    futures::executor::block_on(async {
        let directory = tempfile::tempdir().unwrap();
        let store = FileControlStateStore::open(directory.path()).unwrap();
        let first = empty_generation("durable-first");
        let second = empty_generation("durable-second");
        let failures = Rc::new(RefCell::new(BTreeMap::new()));
        let mut supervisor = DurableGenerationSupervisor::open(
            "app",
            FakeRuntime {
                failures: failures.clone(),
                ..FakeRuntime::default()
            },
            store.clone(),
        )
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

        failures.borrow_mut().insert(
            second.spec.digest().to_owned(),
            ControlPlaneError::HostFailure {
                detail: "Generation task failed".to_owned(),
            },
        );
        let failure = supervisor
            .reconcile_active_generation(160)
            .unwrap()
            .expect("the runtime failure should be observed exactly once");
        assert_eq!(failure.generation_spec_digest, second.spec.digest());
        assert!(
            supervisor
                .reconcile_active_generation(160)
                .unwrap()
                .is_none()
        );
        let rollback = failure
            .automatic_rollback
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

#[test]
fn maintenance_can_reactivate_an_exact_retired_generation() {
    futures::executor::block_on(async {
        let first = empty_generation("reactivate-first");
        let second = empty_generation("reactivate-second");
        let mut supervisor = DurableGenerationSupervisor::open(
            "app",
            FakeRuntime::default(),
            MemoryControlStateStore::default(),
        )
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
        supervisor
            .transition(
                &transition(
                    Some(first.spec.digest()),
                    &second,
                    ReplacementMode::Maintenance,
                    "0",
                ),
                &second,
                &BTreeMap::new(),
                1,
            )
            .await
            .unwrap();

        supervisor
            .transition(
                &transition(
                    Some(second.spec.digest()),
                    &first,
                    ReplacementMode::Maintenance,
                    "0",
                ),
                &first,
                &BTreeMap::new(),
                2,
            )
            .await
            .unwrap();

        assert_eq!(
            supervisor.state().active_generation_spec_digest.as_deref(),
            Some(first.spec.digest())
        );
        assert_eq!(supervisor.state().generations.len(), 2);
        assert_eq!(
            supervisor
                .state()
                .generations
                .iter()
                .find(|record| record.generation_spec_digest == first.spec.digest())
                .unwrap()
                .lifecycle,
            ControlLifecycle::Active
        );
        assert_eq!(
            supervisor
                .state()
                .generations
                .iter()
                .find(|record| record.generation_spec_digest == second.spec.digest())
                .unwrap()
                .lifecycle,
            ControlLifecycle::Retired
        );
        assert!(supervisor.route().is_ok());
    });
}

#[test]
fn recovery_fences_the_old_supervisor_and_rolls_back_failed_active_staging() {
    futures::executor::block_on(async {
        let directory = tempfile::tempdir().unwrap();
        let store = FileControlStateStore::open(directory.path()).unwrap();
        let first = empty_generation("recovery-first");
        let second = empty_generation("recovery-second");
        let mut old =
            DurableGenerationSupervisor::open("app", FakeRuntime::default(), store.clone())
                .unwrap();
        old.transition(
            &transition(None, &first, ReplacementMode::Initial, "0"),
            &first,
            &BTreeMap::new(),
            0,
        )
        .await
        .unwrap();
        old.transition(
            &transition_with_receipts(
                Some(first.spec.digest()),
                &second,
                ReplacementMode::Overlap,
                "10000000000",
                true,
                Vec::new(),
            ),
            &second,
            &BTreeMap::new(),
            100,
        )
        .await
        .unwrap();
        old.complete_drain(first.spec.digest(), 150).await.unwrap();

        let stage_failures = Rc::new(RefCell::new(BTreeSet::from([second
            .spec
            .digest()
            .to_owned()])));
        let authorities = BTreeMap::from([
            (first.spec.digest().to_owned(), first.clone()),
            (second.spec.digest().to_owned(), second.clone()),
        ]);
        let recovered = DurableGenerationSupervisor::recover(
            "app",
            FakeRuntime {
                stage_failures,
                ..FakeRuntime::default()
            },
            store,
            &authorities,
            200,
        )
        .await
        .unwrap();

        assert!(old.lease().is_err());
        assert_eq!(
            recovered.state().active_generation_spec_digest.as_deref(),
            Some(first.spec.digest())
        );
        let active = recovered
            .state()
            .generations
            .iter()
            .find(|record| record.generation_spec_digest == first.spec.digest())
            .unwrap();
        assert_eq!(active.lifecycle, ControlLifecycle::Active);
        assert_eq!(active.activation_direction, ActivationDirection::Rollback);
        let failed = recovered
            .state()
            .generations
            .iter()
            .find(|record| record.generation_spec_digest == second.spec.digest())
            .unwrap();
        assert_eq!(failed.lifecycle, ControlLifecycle::Retired);
        assert_eq!(failed.health, ControlHealth::Failed);
        assert!(recovered.lease().is_ok());
    });
}

#[test]
fn controller_routes_rolls_back_during_drain_and_shuts_down_after_leases() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async {
        let failures = Rc::new(RefCell::new(BTreeMap::new()));
        let supervisor = DurableGenerationSupervisor::open(
            "app",
            FakeRuntime {
                failures: failures.clone(),
                ..FakeRuntime::default()
            },
            MemoryControlStateStore::default(),
        )
        .unwrap();
        let (controller, client) =
            GenerationController::new(supervisor, std::time::Duration::from_millis(2)).unwrap();
        let controller_task = tokio::task::spawn_local(controller.run());
        let mut events = client.subscribe();
        let first = empty_generation("controller-first");
        let second = empty_generation("controller-second");
        let initial = client
            .transition(
                transition(None, &first, ReplacementMode::Initial, "0"),
                first.clone(),
                BTreeMap::new(),
            )
            .await
            .unwrap();
        let first_route = client.route().await.unwrap();
        assert_eq!(first_route.target(), first.spec.digest());

        let overlap = transition_with_receipts(
            Some(first.spec.digest()),
            &second,
            ReplacementMode::Overlap,
            "10000000000",
            true,
            Vec::new(),
        );
        let switched = client
            .transition(overlap, second.clone(), BTreeMap::new())
            .await
            .unwrap();
        assert!(client.route_at_epoch(initial.routing_epoch).await.is_err());
        assert_eq!(client.route().await.unwrap().target(), second.spec.digest());

        failures.borrow_mut().insert(
            second.spec.digest().to_owned(),
            ControlPlaneError::HostFailure {
                detail: "terminal candidate".to_owned(),
            },
        );
        let rollback = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if let GenerationControllerEvent::Maintained(
                    GenerationMaintenanceOutcome::Failed(failure),
                ) = events.recv().await.unwrap()
                {
                    break failure
                        .automatic_rollback
                        .expect("the draining predecessor should remain rollback-capable");
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(rollback.routing_epoch, switched.routing_epoch + 1);
        assert_eq!(rollback.active_generation_spec_digest, first.spec.digest());

        let shutdown_client = client.clone();
        let shutdown = tokio::task::spawn_local(async move { shutdown_client.shutdown().await });
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        drop(first_route);
        let state = tokio::time::timeout(std::time::Duration::from_secs(1), shutdown)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            state
                .generations
                .iter()
                .all(|record| record.lifecycle == ControlLifecycle::Retired)
        );
        assert!(controller_task.await.unwrap().is_ok());
    });
}

#[test]
fn controller_suspends_and_recovers_the_same_active_generation() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async {
        let directory = tempfile::tempdir().unwrap();
        let store = FileControlStateStore::open(directory.path()).unwrap();
        let generation = empty_generation("suspend-recover");
        let supervisor =
            DurableGenerationSupervisor::open("app", FakeRuntime::default(), store.clone())
                .unwrap();
        let (controller, client) =
            GenerationController::new(supervisor, std::time::Duration::from_millis(2)).unwrap();
        let task = tokio::task::spawn_local(controller.run());
        client
            .transition(
                transition(None, &generation, ReplacementMode::Initial, "0"),
                generation.clone(),
                BTreeMap::new(),
            )
            .await
            .unwrap();
        let route = client.route().await.unwrap();
        assert!(client.suspend().await.is_err());
        drop(route);
        let suspended = client.suspend().await.unwrap();
        assert_eq!(
            suspended.active_generation_spec_digest.as_deref(),
            Some(generation.spec.digest())
        );
        assert_eq!(suspended.generations[0].lifecycle, ControlLifecycle::Active);
        assert!(suspended.host_suspended);
        assert_eq!(task.await.unwrap().unwrap(), suspended);

        let recovered = DurableGenerationSupervisor::recover(
            "app",
            FakeRuntime::default(),
            store,
            &BTreeMap::from([(generation.spec.digest().to_owned(), generation)]),
            10,
        )
        .await
        .unwrap();
        assert!(!recovered.state().host_suspended);
        assert!(recovered.route().is_ok());
    });
}

#[test]
fn cleanly_suspended_host_can_be_replaced_without_restaging_the_old_build() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async {
        let directory = tempfile::tempdir().unwrap();
        let store = FileControlStateStore::open(directory.path()).unwrap();
        let old_generation = empty_generation("old-host-build");
        let mut old_host =
            DurableGenerationSupervisor::open("app", FakeRuntime::default(), store.clone())
                .unwrap();
        old_host
            .transition(
                &transition(None, &old_generation, ReplacementMode::Initial, "0"),
                &old_generation,
                &BTreeMap::new(),
                0,
            )
            .await
            .unwrap();
        old_host.suspend_host().await.unwrap();

        let mut replacement = DurableGenerationSupervisor::replace_suspended_host(
            "app",
            FakeRuntime::default(),
            store,
        )
        .unwrap();
        assert!(!replacement.state().host_suspended);
        assert!(replacement.state().active_generation_spec_digest.is_none());
        assert_eq!(
            replacement.state().generations[0].retirement_reason,
            Some(RetirementReason::HostBuildReplaced)
        );

        let new_generation = empty_generation("new-host-build");
        replacement
            .transition(
                &transition(None, &new_generation, ReplacementMode::Initial, "0"),
                &new_generation,
                &BTreeMap::new(),
                1,
            )
            .await
            .unwrap();
        assert_eq!(
            replacement.route().unwrap().target(),
            new_generation.spec.digest()
        );
    });
}

#[test]
fn host_replacement_rejects_live_or_unclean_control_state() {
    futures::executor::block_on(async {
        let directory = tempfile::tempdir().unwrap();
        let store = FileControlStateStore::open(directory.path()).unwrap();
        let generation = empty_generation("still-live-host");
        let mut live =
            DurableGenerationSupervisor::open("app", FakeRuntime::default(), store.clone())
                .unwrap();
        live.transition(
            &transition(None, &generation, ReplacementMode::Initial, "0"),
            &generation,
            &BTreeMap::new(),
            0,
        )
        .await
        .unwrap();

        let error = DurableGenerationSupervisor::replace_suspended_host(
            "app",
            FakeRuntime::default(),
            store,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            ControlPlaneError::TransitionRejected { detail }
                if detail.contains("clean durable suspension")
        ));
        assert!(live.route().is_ok());
    });
}

#[test]
fn maintenance_forces_retirement_after_the_durable_drain_deadline() {
    futures::executor::block_on(async {
        let first = empty_generation("deadline-first");
        let second = empty_generation("deadline-second");
        let mut supervisor = DurableGenerationSupervisor::open(
            "app",
            FakeRuntime::default(),
            MemoryControlStateStore::default(),
        )
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
        let route = supervisor.route().unwrap();
        supervisor
            .transition(
                &transition(
                    Some(first.spec.digest()),
                    &second,
                    ReplacementMode::Overlap,
                    "0",
                ),
                &second,
                &BTreeMap::new(),
                0,
            )
            .await
            .unwrap();

        let outcomes = supervisor.maintain(1_000_000_000).await.unwrap();
        assert!(outcomes.iter().any(|outcome| matches!(
            outcome,
            GenerationMaintenanceOutcome::Retired {
                generation_spec_digest,
                reason: RetirementReason::DrainDeadlineExceeded,
            } if generation_spec_digest == first.spec.digest()
        )));
        assert_eq!(route.target(), first.spec.digest());
        let record = supervisor
            .state()
            .generations
            .iter()
            .find(|record| record.generation_spec_digest == first.spec.digest())
            .unwrap();
        assert_eq!(record.lifecycle, ControlLifecycle::Retired);
        assert_eq!(
            record.retirement_reason,
            Some(RetirementReason::DrainDeadlineExceeded)
        );
    });
}

#[test]
fn terminal_active_without_a_live_rollback_target_is_fenced_and_retired() {
    futures::executor::block_on(async {
        let failures = Rc::new(RefCell::new(BTreeMap::new()));
        let generation = empty_generation("terminal-no-rollback");
        let mut supervisor = DurableGenerationSupervisor::open(
            "app",
            FakeRuntime {
                failures: failures.clone(),
                ..FakeRuntime::default()
            },
            MemoryControlStateStore::default(),
        )
        .unwrap();
        let activation = supervisor
            .transition(
                &transition(None, &generation, ReplacementMode::Initial, "0"),
                &generation,
                &BTreeMap::new(),
                0,
            )
            .await
            .unwrap();
        let route = supervisor.route().unwrap();
        failures.borrow_mut().insert(
            generation.spec.digest().to_owned(),
            ControlPlaneError::HostFailure {
                detail: "terminal active".to_owned(),
            },
        );

        let outcomes = supervisor.maintain(1).await.unwrap();
        assert!(outcomes.iter().any(|outcome| matches!(
            outcome,
            GenerationMaintenanceOutcome::Failed(failure)
                if failure.generation_spec_digest == generation.spec.digest()
                    && failure.automatic_rollback.is_none()
        )));
        assert!(supervisor.state().active_generation_spec_digest.is_none());
        assert_eq!(
            supervisor.state().routing_epoch,
            activation.routing_epoch + 1
        );
        assert!(supervisor.route_at_epoch(activation.routing_epoch).is_err());
        assert_eq!(route.target(), generation.spec.digest());
        let record = supervisor
            .state()
            .generations
            .iter()
            .find(|record| record.generation_spec_digest == generation.spec.digest())
            .unwrap();
        assert_eq!(record.lifecycle, ControlLifecycle::Retired);
        assert_eq!(record.health, ControlHealth::Failed);
        assert_eq!(
            record.retirement_reason,
            Some(RetirementReason::TerminalFailure)
        );
    });
}

#[test]
fn terminal_standby_is_retired_and_cannot_receive_automatic_rollback() {
    futures::executor::block_on(async {
        let failures = Rc::new(RefCell::new(BTreeMap::new()));
        let first = empty_generation("failed-standby-first");
        let second = empty_generation("failed-standby-second");
        let mut supervisor = DurableGenerationSupervisor::open(
            "app",
            FakeRuntime {
                failures: failures.clone(),
                ..FakeRuntime::default()
            },
            MemoryControlStateStore::default(),
        )
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
        supervisor
            .transition(
                &transition_with_receipts(
                    Some(first.spec.digest()),
                    &second,
                    ReplacementMode::Overlap,
                    "10000000000",
                    true,
                    Vec::new(),
                ),
                &second,
                &BTreeMap::new(),
                1,
            )
            .await
            .unwrap();
        supervisor.maintain(2).await.unwrap();
        failures.borrow_mut().insert(
            first.spec.digest().to_owned(),
            ControlPlaneError::HostFailure {
                detail: "terminal standby".to_owned(),
            },
        );
        supervisor.maintain(3).await.unwrap();
        failures.borrow_mut().insert(
            second.spec.digest().to_owned(),
            ControlPlaneError::HostFailure {
                detail: "terminal active".to_owned(),
            },
        );
        let outcomes = supervisor.maintain(4).await.unwrap();
        assert!(outcomes.iter().any(|outcome| matches!(
            outcome,
            GenerationMaintenanceOutcome::Failed(failure)
                if failure.generation_spec_digest == second.spec.digest()
                    && failure.automatic_rollback.is_none()
        )));
        assert!(supervisor.state().active_generation_spec_digest.is_none());
        assert!(supervisor.state().generations.iter().all(|record| {
            record.lifecycle == ControlLifecycle::Retired && record.health == ControlHealth::Failed
        }));
    });
}

#[test]
fn shutdown_does_not_return_an_existing_drain_to_standby() {
    futures::executor::block_on(async {
        let first = empty_generation("shutdown-drain-first");
        let second = empty_generation("shutdown-drain-second");
        let mut supervisor = DurableGenerationSupervisor::open(
            "app",
            FakeRuntime::default(),
            MemoryControlStateStore::default(),
        )
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
        let route = supervisor.route().unwrap();
        supervisor
            .transition(
                &transition(
                    Some(first.spec.digest()),
                    &second,
                    ReplacementMode::Overlap,
                    "10000000000",
                ),
                &second,
                &BTreeMap::new(),
                1,
            )
            .await
            .unwrap();
        supervisor.begin_shutdown(2).unwrap();
        drop(route);
        supervisor.maintain(3).await.unwrap();
        assert!(supervisor.is_retired());
        assert!(
            supervisor
                .state()
                .generations
                .iter()
                .all(|record| record.lifecycle == ControlLifecycle::Retired)
        );
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

#[derive(Debug)]
struct ReplicatedEmptyCatalog;

impl ReplicatedCatalogFactory for ReplicatedEmptyCatalog {
    fn catalog(
        &self,
        _generation: &ResolvedGeneration,
        _lane: &ExecutionLaneId,
    ) -> Result<ExecutionAdapterCatalog, ControlPlaneError> {
        Ok(ExecutionAdapterCatalog::new())
    }
}

#[derive(Debug)]
struct SlowReplicatedCatalog;

impl ReplicatedCatalogFactory for SlowReplicatedCatalog {
    fn catalog(
        &self,
        _generation: &ResolvedGeneration,
        _lane: &ExecutionLaneId,
    ) -> Result<ExecutionAdapterCatalog, ControlPlaneError> {
        std::thread::sleep(std::time::Duration::from_millis(20));
        Ok(ExecutionAdapterCatalog::new())
    }
}

#[derive(Debug)]
struct ReplicatedFailingCatalog;

impl ReplicatedCatalogFactory for ReplicatedFailingCatalog {
    fn catalog(
        &self,
        _generation: &ResolvedGeneration,
        _lane: &ExecutionLaneId,
    ) -> Result<ExecutionAdapterCatalog, ControlPlaneError> {
        Ok(ExecutionAdapterCatalog::single(
            NativeModuleRegistry::new().with_factory(FailingFactory),
        ))
    }
}

#[derive(Debug)]
struct ConditionalReplicatedCatalog;

impl ReplicatedCatalogFactory for ConditionalReplicatedCatalog {
    fn catalog(
        &self,
        generation: &ResolvedGeneration,
        _lane: &ExecutionLaneId,
    ) -> Result<ExecutionAdapterCatalog, ControlPlaneError> {
        if generation
            .plan
            .module_instances()
            .iter()
            .any(|instance| instance.package_id() == "example.failing")
        {
            Ok(ExecutionAdapterCatalog::single(
                NativeModuleRegistry::new().with_factory(FailingFactory),
            ))
        } else {
            Ok(ExecutionAdapterCatalog::new())
        }
    }
}

#[derive(Debug)]
struct FailingLifecycle;

impl ModuleLifecycle for FailingLifecycle {
    fn activate(&self, context: ActivateContext) -> ModuleFuture {
        context
            .tasks()
            .spawn_local(Box::pin(async {
                tokio::task::yield_now().await;
                panic!("terminal Generation fixture failure");
            }))
            .expect("the Generation task should be admitted");
        Box::pin(async { Ok(()) })
    }
}

#[derive(Debug)]
struct FailingFactory;

impl NativeModuleFactory for FailingFactory {
    fn package_id(&self) -> &'static str {
        "example.failing"
    }

    fn package_version(&self) -> &'static str {
        "1.0.0"
    }

    fn instantiate(
        &self,
        _context: NativeModuleFactoryContext<'_>,
    ) -> Result<NativeModuleInstance, RuntimeFailure> {
        Ok(NativeModuleInstance::with_lifecycle(
            Vec::new(),
            FailingLifecycle,
        ))
    }
}

#[derive(Debug)]
struct FailingCatalog;

impl CatalogFactory for FailingCatalog {
    fn catalog(
        &self,
        _generation: &ResolvedGeneration,
    ) -> Result<ExecutionAdapterCatalog, ControlPlaneError> {
        Ok(ExecutionAdapterCatalog::single(
            NativeModuleRegistry::new().with_factory(FailingFactory),
        ))
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

#[test]
fn lane_local_kernel_generation_runtime_rejects_a_multi_lane_plan() {
    let generation = generation_with_plan("lane-local-misuse", replicated_empty_plan());
    let mut host = KernelGenerationRuntime::new(EmptyCatalog);
    let error = futures::executor::block_on(host.stage(&generation, 1_000_000_000)).unwrap_err();
    assert!(matches!(error, ControlPlaneError::HostFailure { .. }));
}

#[test]
fn kernel_generation_runtime_reports_a_real_terminal_failure_after_ready() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async {
        let plan = ResolvedAppPlan::new(
            vec![
                ModuleInstancePlan::new("failing", "example.failing")
                    .with_package_revision("1.0.0")
                    .with_restart_policy(RestartPolicy::never())
                    .with_criticality(ModuleCriticality::Critical),
            ],
            Vec::new(),
        );
        let generation = generation_with_plan("kernel-failure", plan);
        let mut host = KernelGenerationRuntime::new(FailingCatalog);
        let handle = host.stage(&generation, 1_000_000_000).await.unwrap();

        let failure = loop {
            if let Some(failure) = host.terminal_failure(&handle) {
                break failure;
            }
            tokio::task::yield_now().await;
        };
        assert!(matches!(failure, ControlPlaneError::HostFailure { .. }));
        host.shutdown(handle, 1_000_000_000).await.unwrap();
    });
}

#[test]
fn replicated_generation_runtime_stages_routes_and_stops_the_complete_lane_set() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let plan = replicated_empty_plan();
        let generation = generation_with_plan("replicated", plan);
        let mut host = ReplicatedGenerationRuntime::new(ReplicatedEmptyCatalog);
        let handle = host.stage(&generation, 1_000_000_000).await.unwrap();
        let route = host.route(&handle);
        assert_eq!(route.lane_count(), 2);
        assert!(host.terminal_failure(&handle).is_none());
        host.shutdown(handle, 1_000_000_000).await.unwrap();
        assert!(!route.is_failed());
    });
}

#[test]
fn replicated_generation_runtime_bounds_the_complete_ready_gate() {
    let plan = replicated_empty_plan();
    let generation = generation_with_plan("replicated-timeout", plan);
    let mut host = ReplicatedGenerationRuntime::new(SlowReplicatedCatalog);
    let error = futures::executor::block_on(host.stage(&generation, 1_000_000)).unwrap_err();
    assert!(matches!(error, ControlPlaneError::HostFailure { .. }));
}

#[test]
fn replicated_generation_runtime_surfaces_one_lane_terminal_failure() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let plan = AppComposition::new(
            vec![
                ModuleInstancePlan::new("failing", "example.failing")
                    .with_package_revision("1.0.0")
                    .with_execution_lane(ExecutionLaneId::new("frontend"))
                    .with_restart_policy(RestartPolicy::never())
                    .with_criticality(ModuleCriticality::Critical),
            ],
            Vec::new(),
        )
        .with_execution_lanes(vec![
            ExecutionLanePlan::new("frontend"),
            ExecutionLanePlan::new("workers"),
        ])
        .resolve()
        .unwrap();
        let generation = generation_with_plan("replicated-failure", plan);
        let mut host = ReplicatedGenerationRuntime::new(ReplicatedFailingCatalog);
        let handle = host.stage(&generation, 1_000_000_000).await.unwrap();
        let failure = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if let Some(failure) = host.terminal_failure(&handle) {
                    break failure;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert!(matches!(failure, ControlPlaneError::HostFailure { .. }));
        host.shutdown(handle, 1_000_000_000).await.unwrap();
    });
}

#[test]
fn controller_automatically_rolls_back_a_terminal_replicated_generation() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async {
        let first = generation_with_plan("replicated-controller-first", replicated_empty_plan());
        let failing_plan = AppComposition::new(
            vec![
                ModuleInstancePlan::new("failing", "example.failing")
                    .with_package_revision("1.0.0")
                    .with_execution_lane(ExecutionLaneId::new("frontend"))
                    .with_restart_policy(RestartPolicy::never())
                    .with_criticality(ModuleCriticality::Critical),
            ],
            Vec::new(),
        )
        .with_execution_lanes(vec![
            ExecutionLanePlan::new("frontend"),
            ExecutionLanePlan::new("workers"),
        ])
        .resolve()
        .unwrap();
        let second = generation_with_plan("replicated-controller-second", failing_plan);
        let supervisor = DurableGenerationSupervisor::open(
            "app",
            ReplicatedGenerationRuntime::new(ConditionalReplicatedCatalog),
            MemoryControlStateStore::default(),
        )
        .unwrap();
        let (controller, client) =
            GenerationController::new(supervisor, std::time::Duration::from_millis(2)).unwrap();
        let task = tokio::task::spawn_local(controller.run());
        let mut events = client.subscribe();
        client
            .transition(
                transition(None, &first, ReplacementMode::Initial, "0"),
                first.clone(),
                BTreeMap::new(),
            )
            .await
            .unwrap();
        let old_route = client.route().await.unwrap();
        assert_eq!(old_route.target().lane_count(), 2);
        client
            .transition(
                transition_with_receipts(
                    Some(first.spec.digest()),
                    &second,
                    ReplacementMode::Overlap,
                    "10000000000",
                    true,
                    Vec::new(),
                ),
                second,
                BTreeMap::new(),
            )
            .await
            .unwrap();

        let rollback = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if let GenerationControllerEvent::Maintained(
                    GenerationMaintenanceOutcome::Failed(failure),
                ) = events.recv().await.unwrap()
                {
                    break failure.automatic_rollback.unwrap();
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(rollback.active_generation_spec_digest, first.spec.digest());
        drop(old_route);
        client.shutdown().await.unwrap();
        assert!(task.await.unwrap().is_ok());
    });
}

fn replicated_empty_plan() -> ResolvedAppPlan {
    AppComposition::new(Vec::new(), Vec::new())
        .with_execution_lanes(vec![
            ExecutionLanePlan::new("frontend"),
            ExecutionLanePlan::new("workers"),
        ])
        .resolve()
        .unwrap()
}

fn empty_generation(marker: &str) -> ResolvedGeneration {
    generation_with_plan(marker, ResolvedAppPlan::empty())
}

fn generation_with_plan(marker: &str, plan: ResolvedAppPlan) -> ResolvedGeneration {
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
