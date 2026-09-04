use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

use futures::FutureExt as _;
use lenso_app_plan::ResolvedAppPlan;
use lenso_runtime_codec::{ArtifactCatalog, InstanceResourceCatalog};

use super::*;
use crate::{
    AppGenerationSpec, EffectiveHostGrantSet, ReplacementMode, ResolvedArtifactSet, RolloutPolicy,
};

#[path = "host_suspension_tests.rs"]
mod suspension;

#[derive(Clone, Debug, Eq, PartialEq)]
enum RuntimeEvent {
    Staged(String),
    Shutdown(String, u64),
}

#[derive(Clone, Debug)]
struct RecordingRuntime {
    events: Rc<RefCell<Vec<RuntimeEvent>>>,
    shutdown_fails: bool,
    shutdown_delay: std::time::Duration,
}

impl RecordingRuntime {
    fn new(shutdown_fails: bool) -> (Self, Rc<RefCell<Vec<RuntimeEvent>>>) {
        let events = Rc::new(RefCell::new(Vec::new()));
        (
            Self {
                events: Rc::clone(&events),
                shutdown_fails,
                shutdown_delay: std::time::Duration::ZERO,
            },
            events,
        )
    }
}

impl GenerationRuntime for RecordingRuntime {
    type Handle = String;
    type Route = String;

    fn stage<'a>(
        &'a mut self,
        generation: &'a ResolvedGeneration,
        _: u64,
    ) -> futures::future::LocalBoxFuture<'a, Result<Self::Handle, ControlPlaneError>> {
        let digest = generation.spec.digest().to_owned();
        self.events
            .borrow_mut()
            .push(RuntimeEvent::Staged(digest.clone()));
        async move { Ok(digest) }.boxed_local()
    }

    fn shutdown(
        &mut self,
        handle: Self::Handle,
        timeout: u64,
    ) -> futures::future::LocalBoxFuture<'_, Result<(), ControlPlaneError>> {
        self.events
            .borrow_mut()
            .push(RuntimeEvent::Shutdown(handle, timeout));
        let fails = self.shutdown_fails;
        let delay = self.shutdown_delay;
        async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            if fails {
                Err(ControlPlaneError::HostFailure {
                    detail: "injected shutdown failure".to_owned(),
                })
            } else {
                Ok(())
            }
        }
        .boxed_local()
    }

    fn terminal_failure(&self, _: &Self::Handle) -> Option<ControlPlaneError> {
        None
    }

    fn route(&self, handle: &Self::Handle) -> Self::Route {
        handle.clone()
    }
}

#[derive(Clone, Debug)]
struct FailingStore {
    state: Rc<RefCell<Option<DurableControlState>>>,
    calls: Rc<Cell<usize>>,
    fail_on: Rc<Cell<Option<usize>>>,
}

impl FailingStore {
    fn empty() -> Self {
        Self {
            state: Rc::new(RefCell::new(None)),
            calls: Rc::new(Cell::new(0)),
            fail_on: Rc::new(Cell::new(None)),
        }
    }

    fn with_state(state: DurableControlState) -> Self {
        Self {
            state: Rc::new(RefCell::new(Some(state))),
            calls: Rc::new(Cell::new(0)),
            fail_on: Rc::new(Cell::new(None)),
        }
    }

    fn fail_on(&self, call: usize) {
        self.fail_on.set(Some(call));
    }

    fn set_routing_epoch(&self, routing_epoch: u64) {
        self.state
            .borrow_mut()
            .as_mut()
            .expect("opened store has state")
            .routing_epoch = routing_epoch;
    }
}

impl ControlStateStore for FailingStore {
    fn load(&self, app_id: &str) -> Result<DurableControlState, ControlPlaneError> {
        let state = self
            .state
            .borrow()
            .clone()
            .unwrap_or_else(|| DurableControlState::initial(app_id));
        state.validate(app_id)?;
        Ok(state)
    }

    fn compare_and_swap(
        &self,
        app_id: &str,
        expected_revision: u64,
        mut next: DurableControlState,
    ) -> Result<DurableControlState, ControlPlaneError> {
        let call = self.calls.get() + 1;
        self.calls.set(call);
        if self.fail_on.get() == Some(call) {
            return Err(ControlPlaneError::StoreFailure {
                detail: "injected CAS failure".to_owned(),
            });
        }
        let current = self.load(app_id)?;
        if current.revision != expected_revision {
            return Err(ControlPlaneError::StoreFailure {
                detail: "unexpected revision".to_owned(),
            });
        }
        next.revision = expected_revision + 1;
        next.validate(app_id)?;
        *self.state.borrow_mut() = Some(next.clone());
        Ok(next)
    }
}

fn generation(marker: &str) -> ResolvedGeneration {
    let artifact_set = CanonicalDocument::from_value(
        "artifacts",
        ResolvedArtifactSet {
            schema_version: 3,
            resolution_authority_digest: format!("authority-{marker}"),
            host_execution_policy_digest: "policy".to_owned(),
            artifacts: Vec::new(),
            instance_resources: Vec::new(),
        },
    )
    .unwrap();
    let grants = CanonicalDocument::from_value(
        "grants",
        EffectiveHostGrantSet {
            schema_version: 2,
            resolution_authority_digest: format!("authority-{marker}"),
            grants: Vec::new(),
        },
    )
    .unwrap();
    let spec = CanonicalDocument::from_value(
        "generation",
        AppGenerationSpec {
            schema_version: 2,
            app_id: "example.app".to_owned(),
            host_build_manifest_digest: format!("host-{marker}"),
            host_execution_policy_digest: "policy".to_owned(),
            resolved_plan_digest: "plan".to_owned(),
            resolution_authority_digest: format!("authority-{marker}"),
            resolved_artifact_set_digest: artifact_set.digest().to_owned(),
            effective_host_grant_set_digest: grants.digest().to_owned(),
        },
    )
    .unwrap();
    ResolvedGeneration {
        plan: ResolvedAppPlan::new(Vec::new(), Vec::new()),
        artifact_set,
        grants,
        spec,
        artifacts: ArtifactCatalog::new(),
        resources: InstanceResourceCatalog::new(),
        stateful_instances: BTreeMap::new(),
    }
}

fn initial_transition(
    candidate: &ResolvedGeneration,
) -> CanonicalDocument<AppGenerationTransitionSpec> {
    CanonicalDocument::from_value(
        "transition",
        AppGenerationTransitionSpec {
            schema_version: 1,
            app_id: "example.app".to_owned(),
            from_generation_spec_digest: None,
            to_generation_spec_digest: candidate.spec.digest().to_owned(),
            replacement_mode: ReplacementMode::Initial,
            state_compatibility_receipt_digests: Vec::new(),
            rollout_policy: RolloutPolicy {
                ready_timeout_nanos: "10".to_owned(),
                drain_timeout_nanos: "20".to_owned(),
                rollback_window_nanos: "0".to_owned(),
                automatic_rollback_on_generation_failure: false,
            },
        },
    )
    .unwrap()
}

fn record(generation: &ResolvedGeneration, lifecycle: ControlLifecycle) -> GenerationControlRecord {
    GenerationControlRecord {
        generation_spec_digest: generation.spec.digest().to_owned(),
        transition_spec_digest: "transition".to_owned(),
        lifecycle,
        health: ControlHealth::Healthy,
        activation_direction: ActivationDirection::Forward,
        ready_timeout_nanos: "10".to_owned(),
        drain_timeout_nanos: "20".to_owned(),
        drain_deadline_unix_nanos: None,
        rollback_deadline_unix_nanos: (lifecycle == ControlLifecycle::Standby)
            .then(|| "1000".to_owned()),
        automatic_rollback_on_generation_failure: false,
        state_compatibility_receipt_digests: Vec::new(),
        retirement_reason: None,
    }
}

fn recovery_state(
    active: &ResolvedGeneration,
    standby: Option<&ResolvedGeneration>,
) -> DurableControlState {
    let mut generations = vec![record(active, ControlLifecycle::Active)];
    if let Some(standby) = standby {
        generations.push(record(standby, ControlLifecycle::Standby));
    }
    DurableControlState {
        schema_version: 1,
        app_id: "example.app".to_owned(),
        revision: 0,
        supervisor_epoch: 1,
        routing_epoch: 1,
        host_suspended: false,
        active_generation_spec_digest: Some(active.spec.digest().to_owned()),
        generations,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn ready_record_failure_shuts_down_the_staged_generation_and_preserves_store_error() {
    let (runtime, events) = RecordingRuntime::new(true);
    let store = FailingStore::empty();
    store.fail_on(3);
    let mut supervisor = DurableGenerationSupervisor::open("example.app", runtime, store).unwrap();
    let candidate = generation("candidate");

    let error = supervisor
        .transition(
            &initial_transition(&candidate),
            &candidate,
            &BTreeMap::new(),
            0,
        )
        .await
        .unwrap_err();

    assert_eq!(
        error,
        ControlPlaneError::StoreFailure {
            detail: "injected CAS failure".to_owned()
        }
    );
    assert_eq!(
        events.borrow().as_slice(),
        [
            RuntimeEvent::Staged(candidate.spec.digest().to_owned()),
            RuntimeEvent::Shutdown(candidate.spec.digest().to_owned(), 20),
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn active_record_failure_shuts_down_the_ready_generation_and_preserves_store_error() {
    let (runtime, events) = RecordingRuntime::new(true);
    let store = FailingStore::empty();
    store.fail_on(4);
    let mut supervisor = DurableGenerationSupervisor::open("example.app", runtime, store).unwrap();
    let candidate = generation("candidate-active");

    let error = supervisor
        .transition(
            &initial_transition(&candidate),
            &candidate,
            &BTreeMap::new(),
            0,
        )
        .await
        .unwrap_err();

    assert_eq!(
        error,
        ControlPlaneError::StoreFailure {
            detail: "injected CAS failure".to_owned()
        }
    );
    assert_eq!(
        events.borrow().as_slice(),
        [
            RuntimeEvent::Staged(candidate.spec.digest().to_owned()),
            RuntimeEvent::Shutdown(candidate.spec.digest().to_owned(), 20),
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn routing_epoch_overflow_after_stage_shuts_down_the_candidate() {
    let (runtime, events) = RecordingRuntime::new(true);
    let store = FailingStore::empty();
    let mut supervisor =
        DurableGenerationSupervisor::open("example.app", runtime, store.clone()).unwrap();
    store.set_routing_epoch(u64::MAX);
    supervisor.state.routing_epoch = u64::MAX;
    let candidate = generation("overflow");

    let error = supervisor
        .transition(
            &initial_transition(&candidate),
            &candidate,
            &BTreeMap::new(),
            0,
        )
        .await
        .unwrap_err();

    assert!(
        matches!(error, ControlPlaneError::TransitionRejected { detail } if detail.contains("Routing Epoch exhausted"))
    );
    assert_eq!(
        events.borrow().as_slice(),
        [
            RuntimeEvent::Staged(candidate.spec.digest().to_owned()),
            RuntimeEvent::Shutdown(candidate.spec.digest().to_owned(), 20),
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn recovery_authority_error_after_one_stage_cleans_up_the_first_handle() {
    let active = generation("active");
    let missing = generation("missing");
    let store = FailingStore::with_state(recovery_state(&active, Some(&missing)));
    let (runtime, events) = RecordingRuntime::new(false);
    let generations = BTreeMap::from([(active.spec.digest().to_owned(), active.clone())]);

    let error =
        DurableGenerationSupervisor::recover("example.app", runtime, store, &generations, 0)
            .await
            .unwrap_err();

    assert!(
        matches!(error, ControlPlaneError::TransitionRejected { detail } if detail.contains("lacks exact Generation"))
    );
    assert_eq!(
        events.borrow().as_slice(),
        [
            RuntimeEvent::Staged(active.spec.digest().to_owned()),
            RuntimeEvent::Shutdown(active.spec.digest().to_owned(), 20),
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn recovery_parse_error_after_one_stage_cleans_up_the_first_handle() {
    let active = generation("active-parse");
    let standby = generation("standby-parse");
    let mut state = recovery_state(&active, Some(&standby));
    state.generations[1].ready_timeout_nanos = "invalid".to_owned();
    let store = FailingStore::with_state(state);
    let (runtime, events) = RecordingRuntime::new(false);
    let generations = BTreeMap::from([
        (active.spec.digest().to_owned(), active.clone()),
        (standby.spec.digest().to_owned(), standby),
    ]);

    let error =
        DurableGenerationSupervisor::recover("example.app", runtime, store, &generations, 0)
            .await
            .unwrap_err();

    assert!(
        matches!(error, ControlPlaneError::TransitionRejected { detail } if detail.contains("ready timeout"))
    );
    assert_eq!(
        events.borrow().as_slice(),
        [
            RuntimeEvent::Staged(active.spec.digest().to_owned()),
            RuntimeEvent::Shutdown(active.spec.digest().to_owned(), 20),
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn final_recovery_cas_failure_shuts_down_every_recovered_handle() {
    let active = generation("active");
    let store = FailingStore::with_state(recovery_state(&active, None));
    store.fail_on(2);
    let (runtime, events) = RecordingRuntime::new(true);
    let generations = BTreeMap::from([(active.spec.digest().to_owned(), active.clone())]);

    let error =
        DurableGenerationSupervisor::recover("example.app", runtime, store, &generations, 0)
            .await
            .unwrap_err();

    assert_eq!(
        error,
        ControlPlaneError::StoreFailure {
            detail: "injected CAS failure".to_owned()
        }
    );
    assert_eq!(
        events.borrow().as_slice(),
        [
            RuntimeEvent::Staged(active.spec.digest().to_owned()),
            RuntimeEvent::Shutdown(active.spec.digest().to_owned(), 20),
        ]
    );
}
