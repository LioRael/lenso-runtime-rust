#[allow(
    dead_code,
    reason = "The shared G1 Driver includes probe helpers unused by this HTTP host."
)]
#[path = "../../../workers-g1/host/src/driver.rs"]
mod driver;

use bytes::Bytes;
use driver::WorkersDriver;
use http::{HeaderName, HeaderValue, Request};
use lenso_kernel::{CancellationToken, Kernel, ShutdownOutcome};
use lenso_native_adapter::NativePluginRegistry;
use lenso_web_http_parity_fixture::{HttpParityEndpointFactory, plan};
use lenso_web_ingress_plugin::{SessionCookieConfig, WebIngressConfig, WebIngressEventFactory};
use serde::Deserialize;
use std::time::Duration;
use wasm_bindgen::prelude::*;

const BODY_LIMIT: usize = 65_536;
const HEAD_LIMIT: usize = 16_384;

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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpInput {
    method: String,
    uri: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[wasm_bindgen]
pub async fn handle_http(input: String, scope: JsValue) -> Result<String, JsValue> {
    if input.len() > BODY_LIMIT * 4 + HEAD_LIMIT * 6 {
        return Err(error("serialized request exceeds bound"));
    }
    let input: HttpInput = serde_json::from_str(&input).map_err(error)?;
    if input.body.len() > BODY_LIMIT {
        return Err(error("request body exceeds bound"));
    }
    let mut request = Request::builder()
        .method(input.method.as_str())
        .uri(input.uri.as_str())
        .body(Bytes::from(input.body))
        .map_err(error)?;
    for (name, value) in input.headers {
        request.headers_mut().append(
            HeaderName::from_bytes(name.as_bytes()).map_err(error)?,
            HeaderValue::from_str(&value).map_err(error)?,
        );
    }
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
