use bytes::Bytes;
use lenso_kernel::{CancellationToken, Kernel, ShutdownOutcome};
use lenso_native_adapter::NativePluginRegistry;
use lenso_web_http_parity_fixture::{HttpParityEndpointFactory, plan};
use lenso_web_ingress_plugin::{SessionCookieConfig, WebIngressConfig, WebIngressEventFactory};
use lenso_workers_driver::WorkersDriver;
use std::time::Duration;
use wasm_bindgen::prelude::*;

mod request;
use request::{BODY_LIMIT, HEAD_LIMIT, decode_request};

#[wasm_bindgen(raw_module = "../cancellation.mjs")]
extern "C" {
    fn cancellation(scope: &JsValue, callback: &JsValue);
}

struct CancellationGuard {
    scope: JsValue,
    _callback: Closure<dyn FnMut()>,
}
impl CancellationGuard {
    fn new(scope: JsValue, token: CancellationToken) -> Self {
        let callback = Closure::new(move || token.cancel());
        cancellation(&scope, callback.as_ref());
        Self {
            scope,
            _callback: callback,
        }
    }
}
impl Drop for CancellationGuard {
    fn drop(&mut self) {
        cancellation(&self.scope, &JsValue::NULL);
    }
}
struct EventGuard(WorkersDriver);
impl Drop for EventGuard {
    fn drop(&mut self) {
        self.0.request_shutdown();
    }
}
fn error(value: impl std::fmt::Debug) -> JsValue {
    JsValue::from_str(&format!("{value:?}"))
}

#[wasm_bindgen]
pub async fn handle_http(input: String, scope: JsValue) -> Result<String, JsValue> {
    let request = decode_request(&input).map_err(error)?;
    let ingress = WebIngressEventFactory::new();
    let config = WebIngressConfig::default()
        .with_session_cookie(
            SessionCookieConfig::new("__Host-session", "__Host-csrf", "x-csrf-token")
                .map_err(error)?,
        )
        .map_err(error)?
        .with_request_limits(BODY_LIMIT, HEAD_LIMIT)
        .map_err(error)?
        .with_request_timeout(Duration::from_millis(500))
        .map_err(error)?;
    let driver = WorkersDriver::new();
    let _guard = EventGuard(driver.clone());
    let cancellation = CancellationToken::new();
    let _cancellation = CancellationGuard::new(scope, cancellation.clone());
    let app = Kernel::start_native(
        plan(serde_json::to_string(&config).map_err(error)?),
        driver,
        NativePluginRegistry::new()
            .with_factory(HttpParityEndpointFactory)
            .with_factory(ingress.clone()),
    )
    .await
    .map_err(error)?;
    let ready = app.is_ready() && app.is_accepting();
    let response = ingress.handle(request, cancellation.clone()).await;
    let shutdown = app.shutdown(Duration::from_millis(200)).await;
    if shutdown != ShutdownOutcome::Clean {
        return Err(error(shutdown));
    }
    let response = response.map_err(error)?;
    let (parts, body) = response.into_parts();
    // Guard the cross-Wasm serialized response even when a Plugin returns excess bytes.
    if body.len() > BODY_LIMIT {
        return Ok(serde_json::json!({"status":502,"headers":[["content-type","application/json"],["x-content-type-options","nosniff"]],"body":b"{\"error\":\"response_body_too_large\"}".as_slice(),"ready":ready,"shutdown":"clean","cancelled":cancellation.is_cancelled()}).to_string());
    }
    let headers = parts
        .headers
        .iter()
        .map(|(name, value)| {
            Ok((
                name.as_str().to_owned(),
                value.to_str().map_err(error)?.to_owned(),
            ))
        })
        .collect::<Result<Vec<_>, JsValue>>()?;
    Ok(serde_json::json!({"status":parts.status.as_u16(),"headers":headers,"body":body.as_ref(),"ready":ready,"shutdown":"clean","cancelled":cancellation.is_cancelled()}).to_string())
}

// Isolated qualification fault: never a routed business Endpoint.
#[wasm_bindgen]
pub fn trap_probe() {
    core::arch::wasm32::unreachable();
}

mod sessions;

#[wasm_bindgen]
pub fn parity_corpus() -> String {
    lenso_web_http_parity_fixture::CORPUS.to_owned()
}
