use std::{cell::Cell, rc::Rc, time::Duration};

use lenso_kernel::{RuntimeDriver, TaskOutcome};
use lenso_runner::TokioDriver;

#[tokio::test(flavor = "current_thread")]
async fn tokio_driver_executes_timers_cancellation_and_shutdown_on_its_local_lane() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let driver = TokioDriver::new();
            let ran = Rc::new(Cell::new(false));
            let ran_task = ran.clone();
            let timer_driver = driver.clone();
            let timer = driver
                .spawn_local(Box::pin(async move {
                    let deadline = timer_driver.now().saturating_add(Duration::from_millis(1));
                    timer_driver.sleep_until(deadline).await;
                    ran_task.set(true);
                }))
                .expect("Tokio should accept the timer task");
            assert_eq!(timer.await, TaskOutcome::Completed);
            assert!(ran.get());

            let cancelled = driver
                .spawn_local(Box::pin(futures::future::pending()))
                .expect("Tokio should accept the cancellable task");
            cancelled.cancel();
            assert_eq!(cancelled.await, TaskOutcome::Cancelled);

            driver.request_shutdown();
            assert!(driver.shutdown_requested());
        })
        .await;
}
