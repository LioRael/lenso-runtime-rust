//! Product-neutral lifecycle/supervision checks on the real Workers Driver.
use crate::{EventGuard, driver::WorkersDriver, error};
use futures::future::LocalBoxFuture;
use lenso_app_plan::*;
use lenso_kernel::*;
use lenso_runtime_conformance::*;
use std::{cell::RefCell, collections::BTreeMap, rc::Rc, time::Duration};
use wasm_bindgen::prelude::*;

#[derive(Debug, Default)]
struct State {
    events: Vec<String>,
    generations: BTreeMap<String, usize>,
}
#[derive(Clone, Debug)]
struct Fixture {
    state: Rc<RefCell<State>>,
    mode: &'static str,
    id: String,
}
impl Fixture {
    fn record(&self, phase: &str) {
        self.state
            .borrow_mut()
            .events
            .push(format!("{phase}:{}", self.id));
    }
    fn failure(&self, phase: &str) -> RuntimeFailure {
        RuntimeFailure::PluginFailure {
            detail: format!("fixture {phase}:{}", self.id),
        }
    }
}
impl ManagedResource for Fixture {
    fn release(&self) -> ResourceFuture {
        let this = self.clone();
        Box::pin(async move {
            this.record("release");
            if this.mode == "release-failure" {
                return Err(this.failure("release"));
            }
            Ok(())
        })
    }
}
impl PluginLifecycle for Fixture {
    fn prepare(&self, context: PrepareContext) -> PluginFuture {
        let this = self.clone();
        Box::pin(async move {
            this.record("prepare");
            context
                .resources()
                .register(this.clone())
                .map_err(|e| RuntimeFailure::Internal {
                    detail: format!("register: {e:?}"),
                })?;
            if this.mode == "prepare-failure" && this.id.starts_with("consumer") {
                return Err(this.failure("prepare"));
            }
            Ok(())
        })
    }
    fn activate(&self, context: ActivateContext) -> PluginFuture {
        let this = self.clone();
        Box::pin(async move {
            // App readiness stays open while a single Plugin generation restarts.
            if this.id.ends_with("-1") && context.ready_gate().is_open() {
                return Err(this.failure("premature readiness"));
            }
            this.record("activate");
            let cancel = context.cancellation();
            context
                .tasks()
                .spawn_local(Box::pin(async move {
                    cancel.cancelled().await;
                }))
                .map_err(|e| RuntimeFailure::Internal {
                    detail: format!("spawn: {e:?}"),
                })?;
            if this.mode == "activate-failure" && this.id.starts_with("consumer") {
                return Err(this.failure("activate"));
            }
            Ok(())
        })
    }
    fn deactivate(&self, context: DeactivateContext) -> PluginFuture {
        let this = self.clone();
        Box::pin(async move {
            if context.tasks().task_count() != 0 {
                return Err(this.failure("tasks still active"));
            }
            this.record("deactivate");
            this.record(&format!("reason-{:?}", context.reason()));
            if this.mode == "deactivate-failure" {
                return Err(this.failure("deactivate"));
            }
            if this.mode == "shutdown-timeout" {
                std::future::pending::<()>().await;
            }
            Ok(())
        })
    }
}
impl ProbeProvider for Fixture {
    fn probe(
        &self,
        _: InvocationContext,
        request: ProbeRequest,
    ) -> LocalBoxFuture<'static, Result<ProbeResponse, ProbeInvocationError>> {
        let id = self.id.clone();
        Box::pin(async move {
            Ok(ProbeResponse {
                value: format!("{id}:{}", request.value),
            })
        })
    }
}
impl ConformancePluginFactory for Fixture {
    fn package_id(&self) -> &'static str {
        "lenso.g1.lifecycle"
    }
    fn instantiate(
        &self,
        instance: &PluginInstancePlan,
    ) -> Result<ConformancePlugin, RuntimeFailure> {
        let key = instance.instance_key();
        let mut state = self.state.borrow_mut();
        let generation = state.generations.entry(key.into()).or_default();
        *generation += 1;
        let id = format!("{key}-{}", *generation);
        drop(state);
        let this = Self { id, ..self.clone() };
        let endpoints: Vec<Rc<dyn NativeRequestEndpoint>> = if key == "provider" {
            vec![Rc::new(ProbeEndpoint::new(this.clone()))]
        } else {
            vec![]
        };
        Ok(ConformancePlugin::with_lifecycle(endpoints, this))
    }
}
fn plan() -> ResolvedAppPlan {
    AppComposition::new(
        vec![
            PluginInstancePlan::new("provider", "lenso.g1.lifecycle")
                .with_restart_policy(RestartPolicy::on_failure(
                    1,
                    Duration::from_secs(30),
                    Duration::ZERO,
                    Duration::ZERO,
                    Duration::from_millis(100),
                ))
                .with_capability(CapabilityEndpointPlan::new(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                    [PROBE_OPERATION],
                )),
            PluginInstancePlan::new("consumer", "lenso.g1.lifecycle").with_requirement(
                CapabilityRequirementPlan::one(PROBE_CAPABILITY_ID, PROBE_DESCRIPTOR_VERSION),
            ),
        ],
        vec![CapabilityBinding::new(
            "consumer",
            PROBE_CAPABILITY_ID,
            PROBE_DESCRIPTOR_VERSION,
            "provider",
        )],
    )
    .resolve()
    .expect("upstream conformance composition")
}
fn check(value: bool, detail: &str) -> Result<(), JsValue> {
    if value { Ok(()) } else { Err(error(detail)) }
}

#[wasm_bindgen]
pub async fn lifecycle_probe() -> Result<String, JsValue> {
    let mut cases = Vec::new();
    for mode in [
        "normal",
        "prepare-failure",
        "activate-failure",
        "deactivate-failure",
        "release-failure",
        "shutdown-timeout",
        "supervision",
    ] {
        let state = Rc::new(RefCell::new(State::default()));
        let driver = WorkersDriver::new();
        let _guard = EventGuard(driver.clone());
        let adapter = ConformanceExecutionAdapter::new().with_factory(Fixture {
            state: state.clone(),
            mode,
            id: String::new(),
        });
        let started = Kernel::start_native(plan(), driver.clone(), adapter).await;
        if mode == "prepare-failure" || mode == "activate-failure" {
            check(
                matches!(started, Err(RuntimeFailure::PluginFailure { ref detail }) if detail.contains(mode.split('-').next().unwrap())),
                "startup error changed",
            )?;
            let events = &state.borrow().events;
            let cleanup: Vec<_> = events
                .iter()
                .filter(|event| event.starts_with("deactivate:") || event.starts_with("release:"))
                .map(String::as_str)
                .collect();
            check(
                cleanup
                    == [
                        "deactivate:consumer-1",
                        "release:consumer-1",
                        "deactivate:provider-1",
                        "release:provider-1",
                    ],
                "startup rollback order/count changed",
            )?;
            check(
                events
                    .iter()
                    .filter(|event| event.starts_with("reason-StartupRollback:"))
                    .count()
                    == 2,
                "rollback reason changed",
            )?;
            cases.push(mode);
            continue;
        }
        let app = started.map_err(error)?;
        let result: Result<(), JsValue> = async {
            check(app.is_ready() && app.is_accepting(), "not Ready")?;
            let client = ProbeClient::new(app.handle::<Probe>("consumer").map_err(error)?);
            if mode == "supervision" {
                app.report_plugin_failure("provider").map_err(error)?;
                let deadline = driver.now() + Duration::from_millis(200);
                while app.plugin_generation("provider") != Some(2) && driver.now() < deadline {
                    driver.yield_now().await;
                }
                check(
                    app.plugin_generation("provider") == Some(2),
                    &format!(
                        "replacement generation missing: terminal={:?}, events={:?}",
                        app.terminal_failure(),
                        state.borrow().events
                    ),
                )?;
                let response = client
                    .probe(ProbeRequest {
                        value: "stable".into(),
                    })
                    .await
                    .map_err(error)?;
                check(
                    response.value == "provider-2:stable",
                    "stable handle did not switch generation",
                )?;
                app.report_plugin_failure("provider").map_err(error)?;
                let deadline = driver.now() + Duration::from_millis(200);
                while !app.is_failed() && driver.now() < deadline {
                    driver.yield_now().await;
                }
                check(
                    app.terminal_failure()
                        == Some(RuntimeFailure::PluginRestartExhausted {
                            instance: "provider".into(),
                            attempts: 1,
                        }),
                    "restart exhaustion changed",
                )?;
                check(!app.is_accepting(), "terminal failure left admission open")?;
            }
            app.request_shutdown();
            check(!app.is_accepting(), "shutdown left admission open")?;
            check(
                client
                    .probe(ProbeRequest {
                        value: "after-shutdown".into(),
                    })
                    .await
                    == Err(ProbeInvocationError::Runtime(
                        RuntimeFailure::AdmissionClosed,
                    )),
                "shutdown admission error changed",
            )?;
            Ok(())
        }
        .await;
        let outcome = app.shutdown(Duration::from_millis(25)).await;
        result?;
        match mode {
            "shutdown-timeout" => check(
                outcome == ShutdownOutcome::Timeout,
                "timeout reported clean",
            )?,
            "deactivate-failure" | "release-failure" => check(
                matches!(
                    outcome,
                    ShutdownOutcome::RuntimeFailure {
                        error: RuntimeFailure::PluginFailure { .. }
                    }
                ),
                "cleanup failure reported clean",
            )?,
            _ => check(
                outcome == ShutdownOutcome::Clean,
                "ordinary cleanup not clean",
            )?,
        }
        if mode == "normal" {
            let before = state.borrow().events.clone();
            let cleanup: Vec<_> = before
                .iter()
                .filter(|event| event.starts_with("deactivate:") || event.starts_with("release:"))
                .map(String::as_str)
                .collect();
            check(
                cleanup
                    == [
                        "deactivate:consumer-1",
                        "release:consumer-1",
                        "deactivate:provider-1",
                        "release:provider-1",
                    ],
                "shutdown cleanup order changed",
            )?;
            check(
                app.shutdown(Duration::from_millis(25)).await == ShutdownOutcome::Clean,
                "repeated shutdown changed outcome",
            )?;
            check(
                state.borrow().events == before,
                "repeated shutdown repeated cleanup",
            )?;
            check(
                before.iter().filter(|e| e.starts_with("release:")).count() == 2,
                "resource release count changed",
            )?;
        }
        cases.push(mode);
    }
    Ok(serde_json::json!({"lifecycle":"passed","cases":cases}).to_string())
}
