#[cfg(not(all(target_arch = "wasm32", target_os = "wasi", target_env = "p2")))]
mod native_fallback {
    use std::{cell::Cell, collections::BTreeMap, rc::Rc, time::Duration};

    use futures::future::pending;
    use lenso_app_plan::ResolvedAppPlan;
    use lenso_kernel::{
        DriverTask, Kernel, NativeExecutionAdapter, PreparedNativeApp, RuntimeDriver,
        RuntimeFailure, TaskOutcome, TerminalOutcome,
    };
    use lenso_wasip2_driver::WasiDriver;

    #[derive(Debug)]
    struct EmptyAdapter;

    impl NativeExecutionAdapter for EmptyAdapter {
        fn prepare(&self, _plan: &ResolvedAppPlan) -> Result<PreparedNativeApp, RuntimeFailure> {
            Ok(PreparedNativeApp::with_modules(Vec::new(), BTreeMap::new()))
        }
    }

    fn spawn_task(driver: &WasiDriver, task: impl Future<Output = ()> + 'static) -> DriverTask {
        driver
            .spawn_local(Box::pin(task))
            .expect("the WASIp2 local scheduler should accept a task")
    }

    #[test]
    fn wasip2_driver_runs_task_timer_cancellation_readiness_and_shutdown_smoke() {
        let driver = WasiDriver::new();
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
            .expect("the WASIp2 Driver should boot an empty native App");
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

#[cfg(all(target_arch = "wasm32", target_os = "wasi", target_env = "p2"))]
mod wasip2_host {
    use std::{
        cell::{Cell, RefCell},
        collections::BTreeMap,
        rc::Rc,
        time::Duration,
    };

    use futures::{executor::block_on, future::pending};
    use lenso_app_plan::ResolvedAppPlan;
    use lenso_kernel::{
        DriverTask, Kernel, NativeExecutionAdapter, PreparedNativeApp, RuntimeDriver,
        RuntimeFailure, ShutdownOutcome, TaskOutcome, TerminalOutcome,
    };
    use lenso_wasip2_driver::WasiDriver;

    #[derive(Debug)]
    struct EmptyAdapter;

    impl NativeExecutionAdapter for EmptyAdapter {
        fn prepare(&self, _plan: &ResolvedAppPlan) -> Result<PreparedNativeApp, RuntimeFailure> {
            Ok(PreparedNativeApp::with_modules(Vec::new(), BTreeMap::new()))
        }
    }

    type AppSmoke = Result<(bool, bool, ShutdownOutcome), RuntimeFailure>;

    fn spawn_task(driver: &WasiDriver, task: impl Future<Output = ()> + 'static) -> DriverTask {
        driver
            .spawn_root(Box::pin(task))
            .expect("the WASIp2 local scheduler should accept a task")
    }

    fn drive_until(driver: &WasiDriver, ready: impl Fn() -> bool) {
        for _ in 0..1_000_000 {
            driver.pump();
            if ready() {
                return;
            }
        }
        panic!("the WASIp2 host did not make the smoke task ready");
    }

    #[test]
    fn wasip2_host_driver_runs_task_timer_cancellation_readiness_and_shutdown_smoke() {
        let driver = WasiDriver::new();
        let ran = Rc::new(Cell::new(false));
        let ran_task = ran.clone();
        let task = spawn_task(&driver, async move {
            ran_task.set(true);
        });
        driver.pump();
        assert!(ran.get());
        assert_eq!(block_on(task), TaskOutcome::Completed);

        let timer_fired = Rc::new(Cell::new(false));
        let timer_fired_task = timer_fired.clone();
        let timer_driver = driver.clone();
        let deadline = driver.now() + Duration::from_millis(1);
        let timer_task = spawn_task(&driver, async move {
            timer_driver.sleep_until(deadline).await;
            timer_fired_task.set(true);
        });
        driver.pump();
        assert!(!timer_fired.get());
        assert_eq!(driver.next_timer(), Some(deadline));
        drive_until(&driver, || timer_fired.get());
        assert_eq!(block_on(timer_task), TaskOutcome::Completed);

        let cancelled_timer = spawn_task(&driver, {
            let timer_driver = driver.clone();
            async move {
                timer_driver
                    .sleep_until(timer_driver.now() + Duration::from_secs(60))
                    .await;
            }
        });
        driver.pump();
        assert!(driver.next_timer().is_some());
        cancelled_timer.cancel();
        driver.pump();
        assert!(driver.next_timer().is_none());
        assert_eq!(block_on(cancelled_timer), TaskOutcome::Cancelled);

        let cancelled_task = spawn_task(&driver, async {
            pending::<()>().await;
        });
        driver.pump();
        cancelled_task.cancel();
        driver.pump();
        assert_eq!(block_on(cancelled_task), TaskOutcome::Cancelled);

        let app_result = Rc::new(RefCell::new(None::<AppSmoke>));
        let app_result_task = app_result.clone();
        let app_driver = driver.clone();
        driver
            .spawn_root(Box::pin(async move {
                let result = match Kernel::start_native(
                    ResolvedAppPlan::empty(),
                    app_driver.clone(),
                    EmptyAdapter,
                )
                .await
                {
                    Ok(app) => {
                        let ready = app.is_ready();
                        let accepting = app.is_accepting();
                        app.request_shutdown();
                        Ok((ready, accepting, app.shutdown(Duration::from_secs(1)).await))
                    }
                    Err(error) => Err(error),
                };
                *app_result_task.borrow_mut() = Some(result);
            }))
            .expect("the WASIp2 host should accept the Kernel smoke task");
        drive_until(&driver, || app_result.borrow().is_some());
        let (ready, accepting, shutdown) = app_result
            .borrow_mut()
            .take()
            .expect("the Kernel smoke task should report a result")
            .expect("the WASIp2 Driver should boot an empty native App");
        assert!(ready);
        assert!(accepting);
        assert_eq!(shutdown, ShutdownOutcome::Clean);

        driver.request_shutdown();
        let boot_result = Rc::new(RefCell::new(None::<Result<TerminalOutcome, _>>));
        let boot_result_task = boot_result.clone();
        let boot_driver = driver.clone();
        driver
            .spawn_root(Box::pin(async move {
                *boot_result_task.borrow_mut() =
                    Some(Kernel::boot(ResolvedAppPlan::empty(), boot_driver).await);
            }))
            .expect("the WASIp2 host should accept the boot smoke task");
        drive_until(&driver, || boot_result.borrow().is_some());
        assert_eq!(
            boot_result
                .borrow_mut()
                .take()
                .expect("the boot smoke task should report a result"),
            Ok(TerminalOutcome::ShutdownRequested)
        );
    }
}
