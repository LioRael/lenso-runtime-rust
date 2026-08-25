use std::{cell::RefCell, collections::{BTreeMap, VecDeque}};

wit_bindgen::generate!({
    path: "wit",
    world: "plugin",
});

#[derive(Default)]
struct Session {
    messages: VecDeque<String>,
    closed: bool,
}

#[derive(Default)]
struct State {
    next_id: u64,
    sessions: BTreeMap<u64, Session>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State { next_id: 1, sessions: BTreeMap::new() });
}

struct GuestComponent;

impl Guest for GuestComponent {
    fn describe() -> String {
        r#"{"abi":"lenso.json-interactions@1","capabilities":[{"capability_id":"test.chat@1","descriptor_version":"1.0.0","request_operations":[],"stream_operations":["chat"]}]}"#.to_owned()
    }

    fn invoke(_: String, _: String, _: String) -> Result<String, String> {
        Ok("null".to_owned())
    }

    fn stream_open(capability: String, operation: String, request_json: String) -> Result<u64, String> {
        assert_eq!(capability, "test.chat@1");
        assert_eq!(operation, "chat");
        if request_json == "0" {
            return Err("\"rejected\"".to_owned());
        }
        STATE.with_borrow_mut(|state| {
            let id = state.next_id;
            state.next_id += 1;
            state.sessions.insert(id, Session::default());
            Ok(id)
        })
    }

    fn stream_send(stream_id: u64, message_json: String) -> Result<(), String> {
        STATE.with_borrow_mut(|state| {
            state.sessions.get_mut(&stream_id)
                .ok_or_else(|| "unknown stream".to_owned())?
                .messages.push_back(message_json);
            Ok(())
        })
    }

    fn stream_receive(stream_id: u64) -> Result<String, String> {
        STATE.with_borrow_mut(|state| {
            let session = state.sessions.get_mut(&stream_id).ok_or_else(|| "unknown stream".to_owned())?;
            if let Some(message) = session.messages.pop_front() {
                return Ok(format!(r#"{{"kind":"message","value":{message}}}"#));
            }
            if session.closed {
                session.closed = false;
                return Ok(r#"{"kind":"peer-half-closed"}"#.to_owned());
            }
            Ok(r#"{"kind":"terminal-success"}"#.to_owned())
        })
    }

    fn stream_close_send(stream_id: u64) -> Result<(), String> {
        STATE.with_borrow_mut(|state| {
            state.sessions.get_mut(&stream_id)
                .ok_or_else(|| "unknown stream".to_owned())?
                .closed = true;
            Ok(())
        })
    }

    fn stream_cancel(stream_id: u64) {
        STATE.with_borrow_mut(|state| { state.sessions.remove(&stream_id); });
    }
}

export!(GuestComponent);
