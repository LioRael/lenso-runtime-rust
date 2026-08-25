//! Typed, Plan-bound Capability imports for byte-oriented Lenso guest Modules.

use std::{
    collections::{BTreeMap, BTreeSet},
    marker::PhantomData,
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

/// Raw ABI functions supplied by the selected guest Execution Adapter.
pub trait HostImports: Clone + std::fmt::Debug + 'static {
    fn bindings(&self) -> String;
    fn invoke(&self, binding_id: u32, operation: &str, request_json: &str) -> String;
    fn stream_open(&self, binding_id: u32, operation: &str, request_json: &str) -> String;
    fn stream_send(&self, stream_id: u64, message_json: &str) -> String;
    fn stream_receive(&self, stream_id: u64) -> String;
    fn stream_close_send(&self, stream_id: u64) -> String;
    fn stream_cancel(&self, stream_id: u64) -> String;
}

/// Guest-owned Stream sessions with monotonic, non-zero ABI identities.
///
/// Keep one instance in guest-local state and delegate `stream_open`, the
/// message functions, and `stream_cancel` to it instead of reimplementing ID
/// allocation and lookup for every guest Module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuestSessions<T> {
    next_id: u64,
    sessions: BTreeMap<u64, T>,
}

impl<T> Default for GuestSessions<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> GuestSessions<T> {
    /// Creates an empty session table. Stream identity zero is never issued.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            next_id: 1,
            sessions: BTreeMap::new(),
        }
    }

    /// Inserts one session and returns its stable ABI identity.
    ///
    /// Exhaustion is reported explicitly instead of wrapping and aliasing a
    /// live session. In practice it can only occur after issuing `u64::MAX`
    /// identities in one guest instance.
    pub fn insert(&mut self, session: T) -> Result<u64, GuestSessionError> {
        if self.next_id == 0 {
            return Err(GuestSessionError::IdentityExhausted);
        }
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).unwrap_or(0);
        let previous = self.sessions.insert(id, session);
        debug_assert!(previous.is_none(), "monotonic guest session ID collided");
        Ok(id)
    }

    /// Borrows one live session by ABI identity.
    pub fn get_mut(&mut self, id: u64) -> Result<&mut T, GuestSessionError> {
        self.sessions
            .get_mut(&id)
            .ok_or(GuestSessionError::UnknownSession(id))
    }

    /// Removes one live session, returning its owned state.
    pub fn remove(&mut self, id: u64) -> Result<T, GuestSessionError> {
        self.sessions
            .remove(&id)
            .ok_or(GuestSessionError::UnknownSession(id))
    }

    /// Number of live sessions retained by this guest instance.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Whether no live sessions are retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

/// Bounded failures from guest Stream session management.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuestSessionError {
    /// No further non-zero `u64` identity can be issued safely.
    IdentityExhausted,
    /// The Host referenced an identity that is not live in this guest.
    UnknownSession(u64),
}

/// Defines a zero-sized [`HostImports`] implementation over `wit-bindgen` world imports.
#[macro_export]
macro_rules! wasm_host {
    ($visibility:vis struct $name:ident $(;)?) => {
        $crate::wasm_host! {
            $visibility struct $name {
                bindings: host_bindings,
                invoke: host_invoke,
                stream_open: host_stream_open,
                stream_send: host_stream_send,
                stream_receive: host_stream_receive,
                stream_close_send: host_stream_close_send,
                stream_cancel: host_stream_cancel,
            }
        }
    };
    (
        $visibility:vis struct $name:ident {
            bindings: $bindings:path,
            invoke: $invoke:path,
            stream_open: $stream_open:path,
            stream_send: $stream_send:path,
            stream_receive: $stream_receive:path,
            stream_close_send: $stream_close_send:path,
            stream_cancel: $stream_cancel:path $(,)?
        }
    ) => {
        #[derive(Clone, Copy, Debug, Default)]
        $visibility struct $name;

        impl $crate::HostImports for $name {
            fn bindings(&self) -> ::std::string::String {
                $bindings()
            }

            fn invoke(
                &self,
                binding_id: u32,
                operation: &str,
                request_json: &str,
            ) -> ::std::string::String {
                $invoke(binding_id, operation, request_json)
            }

            fn stream_open(
                &self,
                binding_id: u32,
                operation: &str,
                request_json: &str,
            ) -> ::std::string::String {
                $stream_open(binding_id, operation, request_json)
            }

            fn stream_send(&self, stream_id: u64, message_json: &str) -> ::std::string::String {
                $stream_send(stream_id, message_json)
            }

            fn stream_receive(&self, stream_id: u64) -> ::std::string::String {
                $stream_receive(stream_id)
            }

            fn stream_close_send(&self, stream_id: u64) -> ::std::string::String {
                $stream_close_send(stream_id)
            }

            fn stream_cancel(&self, stream_id: u64) -> ::std::string::String {
                $stream_cancel(stream_id)
            }
        }
    };
}

/// A bounded Runtime Failure projection safe for guest code to inspect.
#[derive(Clone, Debug, PartialEq)]
pub struct GuestRuntimeFailure {
    kind: String,
    value: Value,
}

impl GuestRuntimeFailure {
    /// Stable failure category projected by the Host.
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Complete bounded failure value without Host-private details.
    pub const fn value(&self) -> &Value {
        &self.value
    }
}

/// A guest-side protocol or generated-value failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GuestProtocolError {
    InvalidEnvelope,
    InvalidValue,
    MissingBinding {
        capability_id: &'static str,
        descriptor_version: &'static str,
    },
    AmbiguousBinding {
        capability_id: &'static str,
        descriptor_version: &'static str,
    },
    DescriptorMismatch {
        capability_id: &'static str,
        descriptor_version: &'static str,
    },
}

/// Domain, Runtime, and protocol outcomes remain explicitly distinct.
#[derive(Clone, Debug, PartialEq)]
pub enum GuestError<E> {
    Domain(E),
    Runtime(GuestRuntimeFailure),
    Protocol(GuestProtocolError),
}

impl<E> GuestError<E> {
    fn protocol(error: GuestProtocolError) -> Self {
        Self::Protocol(error)
    }
}

/// One exact opaque binding derived from the immutable resolved Plan.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GuestBinding {
    binding_id: u32,
    provider_instance: String,
    capability_id: String,
    descriptor_version: String,
    request_operations: Vec<String>,
    stream_operations: Vec<String>,
}

impl GuestBinding {
    pub const fn binding_id(&self) -> u32 {
        self.binding_id
    }

    pub fn provider_instance(&self) -> &str {
        &self.provider_instance
    }

    pub fn capability_id(&self) -> &str {
        &self.capability_id
    }

    pub fn descriptor_version(&self) -> &str {
        &self.descriptor_version
    }
}

/// Loaded guest imports. Discovery occurs once and contains no ambient lookup.
#[derive(Clone, Debug)]
pub struct GuestContext<H: HostImports> {
    host: H,
    bindings: Vec<GuestBinding>,
}

impl<H: HostImports> GuestContext<H> {
    /// Loads the exact binding table activated for this Module generation.
    pub fn load(host: H) -> Result<Self, GuestError<Value>> {
        let encoded = host.bindings();
        let bindings = decode_transport_success::<Vec<GuestBinding>>(&encoded)?;
        Ok(Self { host, bindings })
    }

    /// Resolves exactly one generated Capability client from Plan authority.
    pub fn require(
        &self,
        capability_id: &'static str,
        descriptor_version: &'static str,
        request_operations: &'static [&'static str],
        stream_operations: &'static [&'static str],
    ) -> Result<GuestCapability<'_, H>, GuestError<Value>> {
        let matches = self
            .bindings
            .iter()
            .filter(|binding| {
                binding.capability_id == capability_id
                    && binding.descriptor_version == descriptor_version
            })
            .collect::<Vec<_>>();
        let [binding] = matches.as_slice() else {
            let error = if matches.is_empty() {
                GuestProtocolError::MissingBinding {
                    capability_id,
                    descriptor_version,
                }
            } else {
                GuestProtocolError::AmbiguousBinding {
                    capability_id,
                    descriptor_version,
                }
            };
            return Err(GuestError::Protocol(error));
        };
        if !operations_match(&binding.request_operations, request_operations)
            || !operations_match(&binding.stream_operations, stream_operations)
        {
            return Err(GuestError::Protocol(
                GuestProtocolError::DescriptorMismatch {
                    capability_id,
                    descriptor_version,
                },
            ));
        }
        Ok(GuestCapability {
            host: &self.host,
            binding,
        })
    }
}

fn operations_match(actual: &[String], expected: &[&str]) -> bool {
    actual.len() == expected.len()
        && actual.iter().map(String::as_str).collect::<BTreeSet<_>>()
            == expected.iter().copied().collect()
}

/// Exact imported Capability used by generated guest clients.
#[derive(Clone, Copy, Debug)]
pub struct GuestCapability<'a, H: HostImports> {
    host: &'a H,
    binding: &'a GuestBinding,
}

impl<H: HostImports> GuestCapability<'_, H> {
    /// Returns the exact opaque Plan binding selected for this client.
    pub const fn binding(&self) -> &GuestBinding {
        self.binding
    }

    /// Invokes one typed Request Operation.
    pub fn request<Request, Response, DomainError>(
        &self,
        operation: &str,
        request: &Request,
    ) -> Result<Response, GuestError<DomainError>>
    where
        Request: Serialize,
        Response: DeserializeOwned,
        DomainError: DeserializeOwned,
    {
        let request = serde_json::to_string(request)
            .map_err(|_| GuestError::protocol(GuestProtocolError::InvalidValue))?;
        decode_invocation(
            &self
                .host
                .invoke(self.binding.binding_id, operation, &request),
        )
    }

    /// Opens one typed bidirectional Stream Operation.
    pub fn open_stream<Request, Message, DomainError>(
        &self,
        operation: &str,
        request: &Request,
    ) -> Result<GuestStream<H, Message, DomainError>, GuestError<DomainError>>
    where
        Request: Serialize,
        Message: Serialize + DeserializeOwned,
        DomainError: DeserializeOwned,
    {
        let request = serde_json::to_string(request)
            .map_err(|_| GuestError::protocol(GuestProtocolError::InvalidValue))?;
        let stream_id = decode_invocation(&self.host.stream_open(
            self.binding.binding_id,
            operation,
            &request,
        ))?;
        Ok(GuestStream {
            host: self.host.clone(),
            stream_id,
            finished: false,
            marker: PhantomData,
        })
    }
}

/// One typed frame received from an imported Host Stream.
#[derive(Clone, Debug, PartialEq)]
pub enum GuestStreamEvent<M, E> {
    Message(M),
    PeerHalfClosed,
    Terminal(Result<(), E>),
}

/// One bounded Host Stream owned by a guest call.
#[derive(Debug)]
pub struct GuestStream<H: HostImports, M, E> {
    host: H,
    stream_id: u64,
    finished: bool,
    marker: PhantomData<fn() -> (M, E)>,
}

impl<H, M, E> GuestStream<H, M, E>
where
    H: HostImports,
    M: Serialize + DeserializeOwned,
    E: DeserializeOwned,
{
    /// Sends one typed message to the Host provider.
    pub fn send(&self, message: &M) -> Result<(), GuestError<Value>> {
        let message = serde_json::to_string(message)
            .map_err(|_| GuestError::protocol(GuestProtocolError::InvalidValue))?;
        decode_transport_success(&self.host.stream_send(self.stream_id, &message))
    }

    /// Receives the next message, peer half-close, or terminal outcome.
    pub fn receive(&mut self) -> Result<GuestStreamEvent<M, E>, GuestError<Value>> {
        let frame =
            decode_transport_success::<WireStreamFrame>(&self.host.stream_receive(self.stream_id))?;
        match frame {
            WireStreamFrame::Message(value) => serde_json::from_value(value)
                .map(GuestStreamEvent::Message)
                .map_err(|_| GuestError::protocol(GuestProtocolError::InvalidValue)),
            WireStreamFrame::PeerHalfClosed => Ok(GuestStreamEvent::PeerHalfClosed),
            WireStreamFrame::TerminalSuccess => {
                self.finished = true;
                Ok(GuestStreamEvent::Terminal(Ok(())))
            }
            WireStreamFrame::TerminalError(value) => {
                self.finished = true;
                serde_json::from_value(value)
                    .map(|error| GuestStreamEvent::Terminal(Err(error)))
                    .map_err(|_| GuestError::protocol(GuestProtocolError::InvalidValue))
            }
        }
    }

    /// Half-closes the guest-to-Host direction.
    pub fn close_send(&self) -> Result<(), GuestError<Value>> {
        decode_transport_success(&self.host.stream_close_send(self.stream_id))
    }

    /// Cancels this Stream and releases its Adapter-local identity.
    pub fn cancel(mut self) -> Result<(), GuestError<Value>> {
        let result = decode_transport_success(&self.host.stream_cancel(self.stream_id));
        self.finished = true;
        result
    }
}

impl<H: HostImports, M, E> Drop for GuestStream<H, M, E> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.host.stream_cancel(self.stream_id);
            self.finished = true;
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
enum WireStreamFrame {
    Message(Value),
    PeerHalfClosed,
    TerminalSuccess,
    TerminalError(Value),
}

fn decode_invocation<T: DeserializeOwned, E: DeserializeOwned>(
    encoded: &str,
) -> Result<T, GuestError<E>> {
    match decode_envelope(encoded)? {
        WireEnvelope::Success(value) => serde_json::from_value(value)
            .map_err(|_| GuestError::protocol(GuestProtocolError::InvalidValue)),
        WireEnvelope::Domain(value) => {
            let error = serde_json::from_value(value)
                .map_err(|_| GuestError::protocol(GuestProtocolError::InvalidValue))?;
            Err(GuestError::Domain(error))
        }
        WireEnvelope::Runtime(error) => Err(GuestError::Runtime(error)),
    }
}

fn decode_transport_success<T: DeserializeOwned>(encoded: &str) -> Result<T, GuestError<Value>> {
    match decode_envelope(encoded)? {
        WireEnvelope::Success(value) => serde_json::from_value(value)
            .map_err(|_| GuestError::protocol(GuestProtocolError::InvalidValue)),
        WireEnvelope::Domain(_) => Err(GuestError::protocol(GuestProtocolError::InvalidEnvelope)),
        WireEnvelope::Runtime(error) => Err(GuestError::Runtime(error)),
    }
}

enum WireEnvelope {
    Success(Value),
    Domain(Value),
    Runtime(GuestRuntimeFailure),
}

fn decode_envelope<E>(encoded: &str) -> Result<WireEnvelope, GuestError<E>> {
    let Value::Object(mut envelope) = serde_json::from_str(encoded)
        .map_err(|_| GuestError::protocol(GuestProtocolError::InvalidEnvelope))?
    else {
        return Err(GuestError::protocol(GuestProtocolError::InvalidEnvelope));
    };
    if envelope.len() != 1 {
        return Err(GuestError::protocol(GuestProtocolError::InvalidEnvelope));
    }
    if let Some(value) = envelope.remove("ok") {
        return Ok(WireEnvelope::Success(value));
    }
    if let Some(value) = envelope.remove("error") {
        return Ok(WireEnvelope::Domain(value));
    }
    let Some(value) = envelope.remove("runtime") else {
        return Err(GuestError::protocol(GuestProtocolError::InvalidEnvelope));
    };
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty())
        .ok_or_else(|| GuestError::protocol(GuestProtocolError::InvalidEnvelope))?
        .to_owned();
    Ok(WireEnvelope::Runtime(GuestRuntimeFailure { kind, value }))
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::VecDeque, rc::Rc};

    use serde_json::json;

    use super::*;

    #[derive(Clone, Debug)]
    struct MockHost {
        responses: Rc<RefCell<VecDeque<String>>>,
        cancelled: Rc<RefCell<Vec<u64>>>,
    }

    impl MockHost {
        fn new(responses: impl IntoIterator<Item = Value>) -> Self {
            Self {
                responses: Rc::new(RefCell::new(
                    responses
                        .into_iter()
                        .map(|value| value.to_string())
                        .collect(),
                )),
                cancelled: Rc::new(RefCell::new(Vec::new())),
            }
        }

        fn next(&self) -> String {
            self.responses.borrow_mut().pop_front().unwrap()
        }
    }

    impl HostImports for MockHost {
        fn bindings(&self) -> String {
            self.next()
        }

        fn invoke(&self, _: u32, _: &str, _: &str) -> String {
            self.next()
        }

        fn stream_open(&self, _: u32, _: &str, _: &str) -> String {
            self.next()
        }

        fn stream_send(&self, _: u64, _: &str) -> String {
            self.next()
        }

        fn stream_receive(&self, _: u64) -> String {
            self.next()
        }

        fn stream_close_send(&self, _: u64) -> String {
            self.next()
        }

        fn stream_cancel(&self, stream_id: u64) -> String {
            self.cancelled.borrow_mut().push(stream_id);
            self.responses
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| json!({ "ok": null }).to_string())
        }
    }

    fn binding() -> Value {
        json!({
            "binding_id": 7,
            "provider_instance": "provider",
            "capability_id": "example.chat@1",
            "descriptor_version": "1.0.0",
            "request_operations": ["inspect"],
            "stream_operations": ["chat"]
        })
    }

    fn raw_bindings() -> String {
        json!({ "ok": [] }).to_string()
    }

    fn raw_invoke(_: u32, _: &str, _: &str) -> String {
        json!({ "ok": null }).to_string()
    }

    fn raw_stream(_: u64, _: &str) -> String {
        json!({ "ok": null }).to_string()
    }

    fn raw_receive(_: u64) -> String {
        json!({ "ok": null }).to_string()
    }

    wasm_host! {
        struct MacroHost {
            bindings: raw_bindings,
            invoke: raw_invoke,
            stream_open: raw_invoke,
            stream_send: raw_stream,
            stream_receive: raw_receive,
            stream_close_send: raw_receive,
            stream_cancel: raw_receive,
        }
    }

    #[test]
    fn wasm_host_macro_connects_generated_world_imports() {
        let host = MacroHost;
        assert_eq!(host.bindings(), json!({ "ok": [] }).to_string());
        assert_eq!(
            host.invoke(0, "operation", "{}"),
            json!({ "ok": null }).to_string()
        );
    }

    #[test]
    fn guest_sessions_issue_non_zero_ids_and_fail_closed_for_unknown_sessions() {
        let mut sessions = GuestSessions::new();
        let first = sessions.insert("first").unwrap();
        let second = sessions.insert("second").unwrap();
        assert_eq!((first, second), (1, 2));
        assert_eq!(sessions.len(), 2);
        assert_eq!(*sessions.get_mut(first).unwrap(), "first");
        assert_eq!(sessions.remove(first).unwrap(), "first");
        assert_eq!(
            sessions.get_mut(first),
            Err(GuestSessionError::UnknownSession(first))
        );
        assert_eq!(
            sessions.remove(0),
            Err(GuestSessionError::UnknownSession(0))
        );
    }

    #[test]
    fn context_selects_one_exact_plan_binding_and_invokes_requests() {
        let host = MockHost::new([
            json!({ "ok": [binding()] }),
            json!({ "ok": { "answer": "ready" } }),
        ]);
        let context = GuestContext::load(host).unwrap();
        let capability = context
            .require("example.chat@1", "1.0.0", &["inspect"], &["chat"])
            .unwrap();
        let response = capability
            .request::<_, Value, Value>("inspect", &json!({ "input": "hello" }))
            .unwrap();
        assert_eq!(response, json!({ "answer": "ready" }));
    }

    #[test]
    fn domain_runtime_and_protocol_failures_remain_distinct() {
        let domain = decode_invocation::<Value, Value>(r#"{"error":{"code":"denied"}}"#);
        assert_eq!(domain, Err(GuestError::Domain(json!({ "code": "denied" }))));

        let runtime = decode_invocation::<Value, Value>(
            r#"{"runtime":{"kind":"deadline-exceeded","request_id":"request-1"}}"#,
        );
        assert!(matches!(
            runtime,
            Err(GuestError::Runtime(error)) if error.kind() == "deadline-exceeded"
        ));

        let protocol = decode_invocation::<Value, Value>(r#"{"ok":null,"error":null}"#);
        assert_eq!(
            protocol,
            Err(GuestError::Protocol(GuestProtocolError::InvalidEnvelope))
        );
    }

    #[test]
    fn typed_stream_receives_terminal_error_and_drop_cancels_live_stream() {
        let host = MockHost::new([
            json!({ "ok": [binding()] }),
            json!({ "ok": 9 }),
            json!({ "ok": { "kind": "message", "value": { "text": "hello" } } }),
            json!({ "ok": { "kind": "terminal-error", "value": { "code": "closed" } } }),
            json!({ "ok": 10 }),
        ]);
        let cancellations = host.cancelled.clone();
        let context = GuestContext::load(host).unwrap();
        let capability = context
            .require("example.chat@1", "1.0.0", &["inspect"], &["chat"])
            .unwrap();
        let mut stream = capability
            .open_stream::<_, Value, Value>("chat", &json!({}))
            .unwrap();
        assert_eq!(
            stream.receive().unwrap(),
            GuestStreamEvent::Message(json!({ "text": "hello" }))
        );
        assert_eq!(
            stream.receive().unwrap(),
            GuestStreamEvent::Terminal(Err(json!({ "code": "closed" })))
        );
        drop(stream);
        assert!(cancellations.borrow().is_empty());

        let live = capability
            .open_stream::<_, Value, Value>("chat", &json!({}))
            .unwrap();
        drop(live);
        assert_eq!(*cancellations.borrow(), [10]);
    }
}
