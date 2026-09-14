mod conformance;
mod diagnostics_proof;
mod driver;
mod lifecycle_proof;
mod named_dependencies_proof;
use driver::WorkersDriver;
use lenso_app_plan::authoring::{
    HostBinding, HostCatalog, HostDefaultPlugin, HostSlot, PluginInstanceId, PluginRootSnapshot,
    resolve_plugin_root,
};
use lenso_capability_http_endpoint as http;
use lenso_kernel::{CancellationToken, Kernel, RuntimeDriver, ShutdownOutcome, TaskOutcome};
use lenso_native_adapter::NativePluginRegistry;
use std::time::Duration;
use wasm_bindgen::prelude::*;

struct EventGuard(WorkersDriver);
impl Drop for EventGuard {
    fn drop(&mut self) {
        self.0.request_shutdown();
    }
}

fn error(value: impl std::fmt::Debug) -> JsValue {
    JsValue::from_str(&format!("{value:?}"))
}

#[wasm_bindgen(raw_module = "../io.mjs")]
extern "C" {
    #[wasm_bindgen(catch)]
    async fn exchange(scope: JsValue, value: String) -> Result<JsValue, JsValue>;
}

#[wasm_bindgen]
pub async fn probe(input: String, mode: String, io_scope: JsValue) -> Result<String, JsValue> {
    if input.len() > 4096 {
        return Err(error("input too large"));
    }
    lenso_workers_g1_echo::link();
    lenso_workers_g1_caller::link();
    let slots = [HostSlot::one("echo"), HostSlot::one("caller")];
    let linked = NativePluginRegistry::host_catalog(slots.clone(), []).map_err(error)?;
    if linked.plugins().len() != 2 {
        return Err(error(format!(
            "expected two generated factories, got {:?}",
            linked
                .plugins()
                .iter()
                .map(|p| p.descriptor().plugin_id())
                .collect::<Vec<_>>()
        )));
    }
    let host = HostCatalog::new(
        slots,
        linked.plugins().to_vec(),
        [
            HostDefaultPlugin::new("lenso.workers-g1.echo", "default")
                .with_configuration(serde_json::json!({"fail": mode == "startup-failure"})),
            HostDefaultPlugin::new("lenso.workers-g1.caller", "default"),
        ],
    )
    .with_bindings([HostBinding::new(
        PluginInstanceId::new("lenso.workers-g1.caller", "default"),
        http::CAPABILITY_ID,
        "echo",
    )]);
    let resolved = resolve_plugin_root(&host, &PluginRootSnapshot::default()).map_err(error)?;
    let driver = WorkersDriver::new();
    let _guard = EventGuard(driver.clone());
    let deadline = driver.now() + Duration::from_millis(5);
    driver.sleep_until(deadline).await;
    if driver.now() < deadline {
        return Err(error("timer completed early"));
    }
    let sleeper = driver.clone();
    let task = driver
        .spawn_root(Box::pin(async move {
            sleeper
                .sleep_until(sleeper.now() + Duration::from_secs(60))
                .await;
        }))
        .map_err(error)?;
    driver.yield_now().await;
    task.cancel();
    if task.await != TaskOutcome::Cancelled {
        return Err(error("cancellation failed"));
    }
    let registry = if mode == "missing-factory" {
        NativePluginRegistry::new()
    } else {
        NativePluginRegistry::new().with_linked_factories()
    };
    let startup = Kernel::start_native(resolved.plan().clone(), driver.clone(), registry).await;
    if mode == "missing-factory" || mode == "startup-failure" {
        return match startup {
            Err(reason) => Ok(serde_json::json!({"rejected":true,"mode":mode,"detail":format!("{reason:?}"),"factories":2}).to_string()),
            Ok(app) => { app.shutdown(Duration::from_secs(1)).await; Err(error("invalid startup reached Ready")) }
        };
    }
    let app = startup.map_err(error)?;
    let ready = app.is_ready() && app.is_accepting();
    #[cfg(feature = "cpu-probe")]
    if mode == "cpu-limit" {
        // Only built for the private, disposable platform-termination probe.
        loop {
            std::hint::black_box(1_u64);
        }
    }
    if mode.starts_with("io-") {
        let io_result = exchange(io_scope, input.clone()).await;
        let value = match io_result {
            Ok(value) => value,
            Err(reason) => {
                app.shutdown(Duration::from_secs(1)).await;
                return Err(reason);
            }
        };
        let value: serde_json::Value = serde_json::from_str(
            &value
                .as_string()
                .ok_or_else(|| error("I/O response must be a string"))?,
        )
        .map_err(error)?;
        if value["state"] == "aborted" {
            let shutdown = app.shutdown(Duration::from_secs(1)).await;
            if shutdown != ShutdownOutcome::Clean {
                return Err(error(shutdown));
            }
            return Ok(serde_json::json!({"io":"aborted","shutdown":"clean"}).to_string());
        }
        if value["state"] != "ok" || value["value"] != input {
            app.shutdown(Duration::from_secs(1)).await;
            return Err(error("I/O identity mismatch"));
        }
    }
    if mode == "task-trap" {
        let task = driver
            .spawn_root(Box::pin(async {
                core::arch::wasm32::unreachable();
            }))
            .map_err(error)?;
        let outcome = task.await;
        app.shutdown(Duration::from_secs(1)).await;
        return Err(error(format!("trap unexpectedly completed: {outcome:?}")));
    }
    let request = http::HandleRequest {
        body: input.clone().into_bytes().into(),
        credential: None,
        headers: vec![],
        method: "POST".into(),
        path: "/probe".into(),
        path_parameters: vec![],
        query: None,
        request_id: input.clone(),
        route_id: mode.clone(),
    };
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let context = app.invocation_context(None, cancelled);
    let rejected = app
        .invoke_with_context::<http::EndpointHandle>(
            "lenso.workers-g1.caller/default",
            http::HANDLE_OPERATION,
            context,
            request.clone(),
        )
        .await;
    if !matches!(
        rejected,
        Err(lenso_kernel::RuntimeFailure::Cancelled { .. })
    ) {
        app.shutdown(Duration::from_secs(1)).await;
        return Err(error("cancelled invocation entered the endpoint"));
    }
    let expired = app.invocation_context(Some(Duration::ZERO), CancellationToken::new());
    let rejected = app
        .invoke_with_context::<http::EndpointHandle>(
            "lenso.workers-g1.caller/default",
            http::HANDLE_OPERATION,
            expired,
            request.clone(),
        )
        .await;
    if !matches!(
        rejected,
        Err(lenso_kernel::RuntimeFailure::DeadlineExceeded { .. })
    ) {
        app.shutdown(Duration::from_secs(1)).await;
        return Err(error("expired invocation was not rejected"));
    }
    let result = app
        .invoke::<http::EndpointHandle>(
            "lenso.workers-g1.caller/default",
            http::HANDLE_OPERATION,
            request,
        )
        .await;
    let shutdown = app.shutdown(Duration::from_secs(1)).await;
    driver.request_shutdown();
    if shutdown != ShutdownOutcome::Clean {
        return Err(error(shutdown));
    }
    let response = result.map_err(error)?.map_err(error)?;
    let body = String::from_utf8(response.body.as_ref().to_vec()).map_err(error)?;
    Ok(serde_json::json!({"ready":ready,"factories":2,"body":body,"invocations":response.headers[0].value,"shutdown":"clean","timer":"passed","cancellation":"passed","invocation_cancellation":"passed","deadline":"passed","elapsed_ms":driver.now().as_millis()}).to_string())
}

// Synchronous trap fixture: the JS boundary must reject, never report a clean run.
#[wasm_bindgen]
pub fn trap_probe() {
    core::arch::wasm32::unreachable();
}

#[wasm_bindgen]
pub async fn driver_probe() -> Result<String, JsValue> {
    let driver = WorkersDriver::new();
    let _guard = EventGuard(driver.clone());
    let mut handles = Vec::new();
    for _ in 0..128 {
        handles.push(
            driver
                .spawn_root(Box::pin(futures::future::pending()))
                .map_err(error)?,
        );
    }
    if driver
        .spawn_root(Box::pin(futures::future::pending()))
        .is_ok()
    {
        return Err(error("task limit not enforced"));
    }
    let parked = driver.wait_for_runtime_event(driver.now() + Duration::from_secs(60));
    // Poll the parking future before requesting shutdown, to exercise wake-up.
    let mut parked = Box::pin(parked);
    use futures::FutureExt;
    if parked.as_mut().now_or_never().is_some() {
        return Err(error("parking did not park"));
    }
    driver.request_shutdown();
    parked.await;
    for handle in handles {
        if handle.await != TaskOutcome::Cancelled {
            return Err(error("task did not cancel"));
        }
    }
    if driver.spawn_root(Box::pin(async {})).is_ok() {
        return Err(error("shutdown admitted a task"));
    }
    Ok(
        serde_json::json!({"task_bound":128,"parking":"passed","shutdown_cancellation":"passed"})
            .to_string(),
    )
}
