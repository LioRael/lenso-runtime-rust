//! Real Wasm response leases for the shared portable duplex fixture.
use super::*;
use futures::{channel::oneshot, future::select};
use lenso_capability_websocket_endpoint as ws;
use lenso_web_ingress_plugin::{
    WebIngressEventBody, WebIngressResponseStream, WebSocketConfig, WebSocketSession,
};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen_futures::future_to_promise;

#[derive(Clone)]
enum Body {
    Buffered(Rc<RefCell<Option<Bytes>>>),
    Stream(Rc<WebIngressResponseStream>),
    Socket(Rc<WebSocketSession>),
}
impl Body {
    fn cancel(&self) {
        match self {
            Self::Buffered(bytes) => {
                bytes.borrow_mut().take();
            }
            Self::Stream(stream) => stream.cancel(),
            Self::Socket(socket) => socket.cancel(),
        }
    }
    async fn read(&self) -> Result<JsValue, JsValue> {
        let bytes = match self {
            Self::Buffered(bytes) => bytes.borrow_mut().take(),
            Self::Stream(stream) => stream.receive().await.map_err(error)?,
            Self::Socket(socket) => {
                return match socket.receive().await.map_err(error)? {
                    Some(frame) => js_sys::JSON::parse(
                        &ws::encode_connect_websocket_response(&frame).map_err(error)?,
                    ),
                    None => Ok(JsValue::NULL),
                };
            }
        };
        Ok(bytes.map_or(JsValue::NULL, |bytes| {
            js_sys::Uint8Array::from(bytes.as_ref()).into()
        }))
    }
}
#[wasm_bindgen]
pub struct ResponseSession {
    status: u16,
    headers: String,
    body: Body,
    closed: js_sys::Promise,
    finish: Rc<RefCell<Option<oneshot::Sender<()>>>>,
}
#[wasm_bindgen]
impl ResponseSession {
    #[wasm_bindgen(getter)]
    pub fn status(&self) -> u16 {
        self.status
    }
    #[wasm_bindgen(getter)]
    pub fn headers(&self) -> String {
        self.headers.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn closed(&self) -> js_sys::Promise {
        self.closed.clone()
    }
    pub fn read(&self) -> js_sys::Promise {
        let body = self.body.clone();
        let finish = self.finish.clone();
        future_to_promise(async move {
            let value = body.read().await;
            if match &value {
                Err(_) => true,
                Ok(value) => value.is_null(),
            } {
                if let Some(finish) = finish.borrow_mut().take() {
                    let _ = finish.send(());
                }
            }
            value
        })
    }
    pub fn send(&self, wire: String) -> js_sys::Promise {
        let body = self.body.clone();
        future_to_promise(async move {
            let Body::Socket(socket) = body else {
                return Err(error("response is not duplex"));
            };
            let frame = ws::decode_connect_websocket_response(&wire).map_err(error)?;
            socket.send(frame).await.map_err(error)?;
            Ok(JsValue::UNDEFINED)
        })
    }
}
#[wasm_bindgen]
pub async fn open_http(input: String, scope: JsValue) -> Result<ResponseSession, JsValue> {
    let request = decode_request(&input).map_err(error)?;
    let ingress = WebIngressEventFactory::new();
    let config = WebIngressConfig::default()
        .with_request_limits(BODY_LIMIT, HEAD_LIMIT)
        .map_err(error)?
        .with_request_timeout(Duration::from_secs(30))
        .map_err(error)?
        .with_websocket(WebSocketConfig::new(vec!["https://client.invalid".into()]).map_err(error)?)
        .map_err(error)?;
    let driver = WorkersDriver::new();
    let event = EventGuard(driver.clone());
    let cancellation = CancellationToken::new();
    let cancel_guard = CancellationGuard::new(scope, cancellation.clone());
    let app = Kernel::start_native(
        lenso_web_duplex_fixture::plan(serde_json::to_string(&config).map_err(error)?),
        driver,
        NativePluginRegistry::new()
            .with_factory(lenso_web_duplex_fixture::DuplexFactory)
            .with_factory(ingress.clone()),
    )
    .await
    .map_err(error)?;
    let response = match ingress.handle_response(request, cancellation.clone()).await {
        Ok(response) => response,
        Err(failure) => {
            let outcome = app.shutdown(Duration::from_millis(200)).await;
            return Err(if outcome == ShutdownOutcome::Clean {
                error(failure)
            } else {
                error(outcome)
            });
        }
    };
    let (parts, body) = response.into_parts();
    let body = match body {
        WebIngressEventBody::Buffered(bytes) => Body::Buffered(Rc::new(RefCell::new(Some(bytes)))),
        WebIngressEventBody::Streaming(stream) => Body::Stream(Rc::new(stream)),
        WebIngressEventBody::WebSocket(socket) => Body::Socket(Rc::new(socket)),
    };
    let headers = serde_json::to_string(
        &parts
            .headers
            .iter()
            .map(|(name, value)| Ok((name.as_str(), value.to_str().map_err(error)?)))
            .collect::<Result<Vec<_>, JsValue>>()?,
    )
    .map_err(error)?;
    let (finish, finished) = oneshot::channel();
    let lifetime_body = body.clone();
    let closed = future_to_promise(async move {
        let _event = event;
        let _cancel = cancel_guard;
        let cancelled = cancellation.cancelled();
        futures::pin_mut!(cancelled, finished);
        let _ = select(cancelled, finished).await;
        lifetime_body.cancel();
        let outcome = app.shutdown(Duration::from_millis(200)).await;
        if outcome != ShutdownOutcome::Clean {
            return Err(error(outcome));
        }
        js_sys::JSON::parse("{\"shutdown\":\"clean\"}")
    });
    Ok(ResponseSession {
        status: parts.status.as_u16(),
        headers,
        body,
        closed,
        finish: Rc::new(RefCell::new(Some(finish))),
    })
}
