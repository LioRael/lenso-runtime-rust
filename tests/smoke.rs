#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod native_fallback {
    use std::{cell::Cell, collections::BTreeMap, rc::Rc, time::Duration};

    use futures::future::pending;
    use lenso_app_plan::ResolvedAppPlan;
    use lenso_browser_driver::BrowserDriver;
    use lenso_kernel::{
        DriverTask, Kernel, NativeExecutionAdapter, PreparedNativeApp, RuntimeDriver,
        RuntimeFailure, TaskOutcome, TerminalOutcome,
    };

    #[derive(Debug)]
    struct EmptyAdapter;

    impl NativeExecutionAdapter for EmptyAdapter {
        fn prepare(&self, _plan: &ResolvedAppPlan) -> Result<PreparedNativeApp, RuntimeFailure> {
            Ok(PreparedNativeApp::with_modules(Vec::new(), BTreeMap::new()))
        }
    }

    fn spawn_task(driver: &BrowserDriver, task: impl Future<Output = ()> + 'static) -> DriverTask {
        driver
            .spawn_local(Box::pin(task))
            .expect("the browser local event loop should accept a task")
    }

    #[test]
    fn browser_driver_runs_task_timer_cancellation_readiness_and_shutdown_smoke() {
        let driver = BrowserDriver::new();
        let ran = Rc::new(Cell::new(false));
        let ran_task = ran.clone();
        let task = spawn_task(&driver, async move {
            ran_task.set(true);
        });
        driver.pump();
        assert!(ran.get());
        assert_eq!(driver.join(task), TaskOutcome::Completed);

        let timer_fired = Rc::new(Cell::new(false));
        let timer_fired_task = timer_fired.clone();
        let timer_driver = driver.clone();
        let timer_task = spawn_task(&driver, async move {
            timer_driver.sleep_until(Duration::from_millis(5)).await;
            timer_fired_task.set(true);
        });
        driver.pump();
        assert!(!timer_fired.get());
        driver.advance(Duration::from_millis(5));
        driver.pump();
        assert!(timer_fired.get());
        assert_eq!(driver.join(timer_task), TaskOutcome::Completed);

        let cancelled_task = spawn_task(&driver, async {
            pending::<()>().await;
        });
        driver.pump();
        cancelled_task.cancel();
        assert_eq!(driver.join(cancelled_task), TaskOutcome::Cancelled);

        let app = driver
            .run(Kernel::start_native(
                ResolvedAppPlan::empty(),
                driver.clone(),
                EmptyAdapter,
            ))
            .expect("the browser Driver should boot an empty native App");
        assert!(app.is_ready());
        assert!(app.is_accepting());
        app.request_shutdown();
        assert!(matches!(
            driver.run(app.shutdown(Duration::from_secs(1))),
            lenso_kernel::ShutdownOutcome::Clean
        ));

        driver.request_shutdown();
        assert_eq!(
            driver.run(Kernel::boot(ResolvedAppPlan::empty(), driver.clone())),
            Ok(TerminalOutcome::ShutdownRequested)
        );
    }
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod browser_host {
    use std::{cell::Cell, collections::BTreeMap, rc::Rc, time::Duration};

    use futures::future::pending;
    use lenso_app_plan::ResolvedAppPlan;
    use lenso_browser_driver::BrowserDriver;
    use lenso_kernel::{
        DriverTask, Kernel, NativeExecutionAdapter, PreparedNativeApp, RuntimeDriver,
        RuntimeFailure, ShutdownOutcome, TaskOutcome, TerminalOutcome,
    };
    use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

    wasm_bindgen_test_configure!(run_in_browser);

    #[derive(Debug)]
    struct EmptyAdapter;

    impl NativeExecutionAdapter for EmptyAdapter {
        fn prepare(&self, _plan: &ResolvedAppPlan) -> Result<PreparedNativeApp, RuntimeFailure> {
            Ok(PreparedNativeApp::with_modules(Vec::new(), BTreeMap::new()))
        }
    }

    fn spawn_task(driver: &BrowserDriver, task: impl Future<Output = ()> + 'static) -> DriverTask {
        driver
            .spawn_root(Box::pin(task))
            .expect("the browser local event loop should accept a task")
    }

    #[wasm_bindgen_test(async)]
    async fn browser_host_driver_runs_task_timer_cancellation_readiness_and_shutdown_smoke() {
        let driver = BrowserDriver::new();
        let ran = Rc::new(Cell::new(false));
        let ran_task = ran.clone();
        let task = spawn_task(&driver, async move {
            ran_task.set(true);
        });
        assert_eq!(task.await, TaskOutcome::Completed);
        assert!(ran.get());

        let timer_fired = Rc::new(Cell::new(false));
        let timer_fired_task = timer_fired.clone();
        let timer_driver = driver.clone();
        let deadline = driver.now() + Duration::from_millis(1);
        let timer_task = spawn_task(&driver, async move {
            timer_driver.sleep_until(deadline).await;
            timer_fired_task.set(true);
        });
        assert_eq!(timer_task.await, TaskOutcome::Completed);
        assert!(timer_fired.get());

        let cancelled_timer = spawn_task(&driver, {
            let timer_driver = driver.clone();
            async move {
                timer_driver
                    .sleep_until(timer_driver.now() + Duration::from_secs(60))
                    .await;
            }
        });
        driver.yield_now().await;
        cancelled_timer.cancel();
        assert_eq!(cancelled_timer.await, TaskOutcome::Cancelled);

        let cancelled_task = spawn_task(&driver, async {
            pending::<()>().await;
        });
        cancelled_task.cancel();
        assert_eq!(cancelled_task.await, TaskOutcome::Cancelled);

        let app = Kernel::start_native(ResolvedAppPlan::empty(), driver.clone(), EmptyAdapter)
            .await
            .expect("the browser Driver should boot an empty native App");
        assert!(app.is_ready());
        assert!(app.is_accepting());
        app.request_shutdown();
        assert_eq!(
            app.shutdown(Duration::from_secs(1)).await,
            ShutdownOutcome::Clean
        );

        driver.request_shutdown();
        assert_eq!(
            Kernel::boot(ResolvedAppPlan::empty(), driver).await,
            Ok(TerminalOutcome::ShutdownRequested)
        );
    }
}
