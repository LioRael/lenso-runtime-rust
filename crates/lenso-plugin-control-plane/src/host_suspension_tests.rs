use std::time::Duration;

use super::*;
use crate::{GenerationController, GenerationControllerEvent};

async fn active_supervisor(
    fails: bool,
) -> (
    DurableGenerationSupervisor<RecordingRuntime, FailingStore>,
    FailingStore,
    Rc<RefCell<Vec<RuntimeEvent>>>,
    ResolvedGeneration,
) {
    let (runtime, events) = RecordingRuntime::new(fails);
    let store = FailingStore::empty();
    let mut supervisor =
        DurableGenerationSupervisor::open("example.app", runtime, store.clone()).unwrap();
    let candidate = generation("suspension");
    supervisor
        .transition(
            &initial_transition(&candidate),
            &candidate,
            &BTreeMap::new(),
            0,
        )
        .await
        .unwrap();
    (supervisor, store, events, candidate)
}

#[tokio::test(flavor = "current_thread")]
async fn suspension_fences_routes_joins_callers_and_recovers_exact_active_generation() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (supervisor, store, events, candidate) = active_supervisor(false).await;
            let lease = supervisor.lease().unwrap();
            let (controller, client) =
                GenerationController::new(supervisor, Duration::from_secs(1)).unwrap();
            let mut notifications = client.subscribe();
            let task = tokio::task::spawn_local(controller.run());
            let first_client = client.clone();
            let first = tokio::task::spawn_local(async move {
                first_client.drain_and_suspend(Duration::from_secs(2)).await
            });
            assert_eq!(
                notifications.recv().await.unwrap(),
                GenerationControllerEvent::SuspensionStarted
            );
            assert!(client.route().await.is_err());
            assert!(client.inspect().await.is_err());
            assert_eq!(events.borrow().len(), 1, "lease must hold execution alive");
            first.abort(); // Cancels only this waiter, not the Controller operation.
            drop(lease);
            let state = client
                .drain_and_suspend(Duration::from_secs(10))
                .await
                .unwrap();
            assert_eq!(task.await.unwrap().unwrap(), state);
            assert_eq!(
                client
                    .drain_and_suspend(Duration::from_secs(10))
                    .await
                    .unwrap(),
                state
            );
            assert!(state.host_suspended);
            assert_eq!(
                state.active_generation_spec_digest.as_deref(),
                Some(candidate.spec.digest())
            );
            assert_eq!(state.generations[0].lifecycle, ControlLifecycle::Active);
            assert_eq!(events.borrow().len(), 2, "cleanup happens once");
            let (runtime, _) = RecordingRuntime::new(false);
            let mut recovered = DurableGenerationSupervisor::recover(
                "example.app",
                runtime,
                store,
                &BTreeMap::from([(candidate.spec.digest().to_owned(), candidate.clone())]),
                0,
            )
            .await
            .unwrap();
            assert_eq!(
                recovered.route().unwrap().generation_spec_digest(),
                candidate.spec.digest()
            );
            recovered.suspend_host().await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn repeated_stop_does_not_extend_deadline_or_record_clean_suspension() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (supervisor, store, events, _) = active_supervisor(false).await;
            let lease = supervisor.lease().unwrap();
            let (controller, client) =
                GenerationController::new(supervisor, Duration::from_secs(1)).unwrap();
            let task = tokio::task::spawn_local(controller.run());
            let (first, second) = tokio::time::timeout(Duration::from_secs(2), async {
                tokio::join!(
                    client.drain_and_suspend(Duration::from_millis(20)),
                    client.drain_and_suspend(Duration::from_secs(60))
                )
            })
            .await
            .expect("second request must not extend the first deadline");
            assert_eq!(first, second);
            assert!(
                first
                    .unwrap_err()
                    .to_string()
                    .contains("termination is unconfirmed")
            );
            assert!(task.await.unwrap().is_err());
            assert!(!store.load("example.app").unwrap().host_suspended);
            assert_eq!(events.borrow().len(), 1);
            assert!(client.route().await.is_err());
            drop(lease);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn cleanup_and_persistence_failures_never_claim_clean_suspension() {
    for cleanup_fails in [false, true] {
        let (mut supervisor, store, _, _) = active_supervisor(cleanup_fails).await;
        if !cleanup_fails {
            store.fail_on(store.calls.get() + 1);
        }
        let result = supervisor
            .drain_and_suspend_host(tokio::time::Instant::now() + Duration::from_secs(1))
            .await;
        assert!(result.is_err());
        assert!(!store.load("example.app").unwrap().host_suspended);
    }
}

#[derive(Debug)]
struct LiveFactory(Rc<Cell<usize>>);

#[tokio::test(flavor = "current_thread")]
async fn cleanup_of_multiple_generations_shares_one_budget() {
    let active = generation("active-budget");
    let standby = generation("standby-budget");
    let store = FailingStore::with_state(recovery_state(&active, Some(&standby)));
    let (mut runtime, events) = RecordingRuntime::new(false);
    runtime.shutdown_delay = Duration::from_millis(100);
    let mut supervisor = DurableGenerationSupervisor::recover(
        "example.app",
        runtime,
        store.clone(),
        &BTreeMap::from([
            (active.spec.digest().to_owned(), active),
            (standby.spec.digest().to_owned(), standby),
        ]),
        0,
    )
    .await
    .unwrap();
    let error = supervisor
        .drain_and_suspend_host(tokio::time::Instant::now() + Duration::from_millis(150))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("termination is unconfirmed"));
    assert!(!store.load("example.app").unwrap().host_suspended);
    let budgets = events
        .borrow()
        .iter()
        .filter_map(|event| match event {
            RuntimeEvent::Shutdown(_, budget) => Some(*budget),
            RuntimeEvent::Staged(_) => None,
        })
        .collect::<Vec<_>>();
    assert!(!budgets.is_empty());
    if budgets.len() == 2 {
        assert!(budgets[1] < budgets[0]);
    }
}

#[derive(Debug)]
struct LiveAdapter(Rc<Cell<usize>>);

#[derive(Debug)]
struct LiveLifecycle(Rc<Cell<usize>>);

impl lenso_kernel::PluginLifecycle for LiveLifecycle {
    fn activate(&self, _: lenso_kernel::ActivateContext) -> lenso_kernel::PluginFuture {
        self.0.set(self.0.get() + 1);
        Box::pin(async { Ok(()) })
    }
    fn deactivate(&self, _: lenso_kernel::DeactivateContext) -> lenso_kernel::PluginFuture {
        self.0.set(self.0.get() - 1);
        Box::pin(async { Ok(()) })
    }
}

impl lenso_kernel::ExecutionAdapter for LiveAdapter {
    fn execution_class(&self) -> lenso_app_plan::ExecutionClassId {
        lenso_app_plan::ExecutionClassId::native_rust()
    }
    fn prepare(
        &self,
        _: &ResolvedAppPlan,
    ) -> Result<lenso_kernel::PreparedNativeApp, lenso_kernel::RuntimeFailure> {
        Ok(lenso_kernel::PreparedNativeApp::new(
            Vec::new(),
            BTreeMap::from([(
                "worker".to_owned(),
                lenso_kernel::PreparedNativePlugin::new(Vec::new(), LiveLifecycle(self.0.clone())),
            )]),
        ))
    }
}

impl crate::CatalogFactory for LiveFactory {
    fn catalog(
        &self,
        _: &ResolvedGeneration,
    ) -> Result<lenso_kernel::ExecutionAdapterCatalog, ControlPlaneError> {
        Ok(lenso_kernel::ExecutionAdapterCatalog::new()
            .with_adapter(LiveAdapter(self.0.clone()))
            .unwrap())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn real_kernel_ready_cleanup_and_recovery_preserve_restartable_intent() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let live = Rc::new(Cell::new(0));
            let store = FailingStore::empty();
            let runtime = crate::KernelGenerationRuntime::new(LiveFactory(live.clone()));
            let mut supervisor =
                DurableGenerationSupervisor::open("example.app", runtime, store.clone()).unwrap();
            let mut candidate = generation("real-kernel");
            candidate.plan = ResolvedAppPlan::new(
                vec![lenso_app_plan::PluginInstancePlan::new("worker", "fixture")],
                Vec::new(),
            );
            let mut edge = initial_transition(&candidate).value().clone();
            edge.rollout_policy.ready_timeout_nanos = "1000000000".to_owned();
            edge.rollout_policy.drain_timeout_nanos = "1000000000".to_owned();
            supervisor
                .transition(
                    &CanonicalDocument::from_value("transition", edge).unwrap(),
                    &candidate,
                    &BTreeMap::new(),
                    0,
                )
                .await
                .unwrap();
            assert_eq!(live.get(), 1, "Ready Gate activated the real Plugin");
            let route = supervisor.route().unwrap();
            let (controller, client) =
                GenerationController::new(supervisor, Duration::from_secs(1)).unwrap();
            let task = tokio::task::spawn_local(controller.run());
            drop(route);
            let stopped = client
                .drain_and_suspend(Duration::from_secs(1))
                .await
                .unwrap();
            assert!(stopped.host_suspended);
            task.await.unwrap().unwrap();
            assert_eq!(live.get(), 0, "real Kernel cleanup completed");
            let runtime = crate::KernelGenerationRuntime::new(LiveFactory(live.clone()));
            let mut recovered = DurableGenerationSupervisor::recover(
                "example.app",
                runtime,
                store,
                &BTreeMap::from([(candidate.spec.digest().to_owned(), candidate)]),
                0,
            )
            .await
            .unwrap();
            assert_eq!(live.get(), 1, "recovery reactivated exact Generation");
            recovered
                .drain_and_suspend_host(tokio::time::Instant::now() + Duration::from_secs(1))
                .await
                .unwrap();
            assert_eq!(live.get(), 0);
        })
        .await;
}
