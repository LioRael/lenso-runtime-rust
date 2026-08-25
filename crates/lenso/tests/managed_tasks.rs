use std::{cell::Cell, cell::RefCell, time::Duration};

use lenso::prelude::*;
use lenso_app_plan::{ModuleInstancePlan, ResolvedAppPlan};
use lenso_kernel::ShutdownOutcome;
use lenso_test::TestApp;

thread_local! {
    static ACTIVATION_FAILS: Cell<bool> = const { Cell::new(false) };
    static TASKS: RefCell<Option<ManagedTasks>> = const { RefCell::new(None) };
    static TASK_OBSERVED_CANCELLATION: Cell<bool> = const { Cell::new(false) };
    static DEACTIVATE_OBSERVED_INACTIVE: Cell<bool> = const { Cell::new(false) };
}

#[module(consumer, lifecycle)]
#[derive(Clone, Debug)]
struct Worker {
    #[tasks]
    tasks: ManagedTasks,
}

impl Lifecycle for Worker {
    async fn activate(&self, _context: ActivateContext) -> Result<(), RuntimeFailure> {
        std::future::ready(()).await;
        assert!(self.tasks.is_active());
        TASKS.with(|tasks| tasks.replace(Some(self.tasks.clone())));
        if ACTIVATION_FAILS.get() {
            return Err(RuntimeFailure::ModuleFailure {
                detail: "configured activation failure".to_owned(),
            });
        }
        let cancellation = self
            .tasks
            .cancellation()
            .expect("activation connected tasks");
        self.tasks
            .spawn_local(async move {
                cancellation.cancelled().await;
                TASK_OBSERVED_CANCELLATION.set(true);
            })
            .expect("the active generation should admit managed work");
        Ok(())
    }

    async fn deactivate(&self, _context: DeactivateContext) -> Result<(), RuntimeFailure> {
        std::future::ready(()).await;
        DEACTIVATE_OBSERVED_INACTIVE.set(!self.tasks.is_active());
        assert!(matches!(
            self.tasks.spawn_local(async {}),
            Err(ManagedTasksError::Inactive)
        ));
        Ok(())
    }
}

fn plan() -> ResolvedAppPlan {
    ResolvedAppPlan::new(vec![ModuleInstancePlan::new("worker", "lenso")], Vec::new())
}

fn reset() {
    ACTIVATION_FAILS.set(false);
    TASKS.with(RefCell::take);
    TASK_OBSERVED_CANCELLATION.set(false);
    DEACTIVATE_OBSERVED_INACTIVE.set(false);
}

#[test]
fn task_field_tracks_activation_failure_cancellation_and_deactivation() {
    reset();
    ACTIVATION_FAILS.set(true);
    let error = TestApp::builder(plan())
        .with_linked_factories()
        .start()
        .unwrap_err();
    assert!(matches!(error, RuntimeFailure::ModuleFailure { .. }));
    TASKS.with(|tasks| {
        assert!(!tasks.borrow().as_ref().unwrap().is_active());
    });

    reset();
    let app = TestApp::builder(plan())
        .with_linked_factories()
        .start()
        .unwrap();
    TASKS.with(|tasks| assert!(tasks.borrow().as_ref().unwrap().is_active()));
    assert_eq!(app.shutdown(Duration::from_secs(1)), ShutdownOutcome::Clean);
    assert!(TASK_OBSERVED_CANCELLATION.get());
    assert!(DEACTIVATE_OBSERVED_INACTIVE.get());
    TASKS.with(|tasks| {
        assert!(!tasks.borrow().as_ref().unwrap().is_active());
    });
}
