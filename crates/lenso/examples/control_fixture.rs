//! Real private-control fixture used by cross-language distribution tests.
#![cfg(feature = "host")]

use futures::FutureExt;
use lenso::host::{
    self, CanonicalDocument, ControlPlaneError, GenerationRuntime, HostBuilder,
    MemoryControlStateStore, ResolvedGeneration,
};
use lenso_app_plan::ResolvedAppPlan;
use lenso_plugin_control_plane::{
    AppGenerationSpec, AppGenerationTransitionSpec, EffectiveHostGrantSet, ReplacementMode,
    ResolvedArtifactSet, RolloutPolicy,
};
use lenso_runtime_codec::{ArtifactCatalog, InstanceResourceCatalog};
use std::{collections::BTreeMap, time::Duration};

#[derive(Debug)]
struct Runtime;
impl GenerationRuntime for Runtime {
    type Handle = ();
    type Route = ();
    fn stage<'a>(
        &'a mut self,
        _: &'a ResolvedGeneration,
        _: u64,
    ) -> futures::future::LocalBoxFuture<'a, Result<(), ControlPlaneError>> {
        async { Ok(()) }.boxed_local()
    }
    fn shutdown(
        &mut self,
        (): (),
        _: u64,
    ) -> futures::future::LocalBoxFuture<'_, Result<(), ControlPlaneError>> {
        async { Ok(()) }.boxed_local()
    }
    fn terminal_failure(&self, (): &()) -> Option<ControlPlaneError> {
        None
    }
    fn route(&self, (): &()) {}
}

fn generation() -> ResolvedGeneration {
    let artifacts = CanonicalDocument::from_value(
        "artifacts",
        ResolvedArtifactSet {
            schema_version: 3,
            resolution_authority_digest: "authority".into(),
            host_execution_policy_digest: "policy".into(),
            artifacts: vec![],
            instance_resources: vec![],
        },
    )
    .unwrap();
    let grants = CanonicalDocument::from_value(
        "grants",
        EffectiveHostGrantSet {
            schema_version: 2,
            resolution_authority_digest: "authority".into(),
            grants: vec![],
        },
    )
    .unwrap();
    let spec = CanonicalDocument::from_value(
        "generation",
        AppGenerationSpec {
            schema_version: 2,
            app_id: "fixture.app".into(),
            host_build_manifest_digest: "fixture".into(),
            host_execution_policy_digest: "policy".into(),
            resolved_plan_digest: "plan".into(),
            resolution_authority_digest: "authority".into(),
            resolved_artifact_set_digest: artifacts.digest().into(),
            effective_host_grant_set_digest: grants.digest().into(),
        },
    )
    .unwrap();
    ResolvedGeneration {
        plan: ResolvedAppPlan::new(vec![], vec![]),
        artifact_set: artifacts,
        grants,
        spec,
        artifacts: ArtifactCatalog::new(),
        resources: InstanceResourceCatalog::new(),
        stateful_instances: BTreeMap::new(),
    }
}

async fn start() -> Result<(host::Host<()>, ResolvedGeneration), ControlPlaneError> {
    let candidate = generation();
    let app =
        HostBuilder::new("fixture.app", Runtime, MemoryControlStateStore::default()).build()?;
    let transition = CanonicalDocument::from_value(
        "transition",
        AppGenerationTransitionSpec {
            schema_version: 1,
            app_id: "fixture.app".into(),
            from_generation_spec_digest: None,
            to_generation_spec_digest: candidate.spec.digest().into(),
            replacement_mode: ReplacementMode::Initial,
            state_compatibility_receipt_digests: vec![],
            rollout_policy: RolloutPolicy {
                ready_timeout_nanos: "1000000000".into(),
                drain_timeout_nanos: "1000000000".into(),
                rollback_window_nanos: "0".into(),
                automatic_rollback_on_generation_failure: false,
            },
        },
    )
    .unwrap();
    app.transition(transition, candidate.clone(), BTreeMap::new())
        .await?;
    Ok((app, candidate))
}

fn main() {
    let distribution = std::env::args().nth(1).expect("distribution identity");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(tokio::task::LocalSet::new().run_until(async {
            let options = host::control::ControlOptions {
                distribution,
                startup_timeout: Duration::from_secs(2),
                stop_timeout: Duration::from_secs(2),
            };
            host::control::serve(options, tokio::io::stdin(), tokio::io::stdout(), start)
                .await
                .unwrap();
        }));
}
