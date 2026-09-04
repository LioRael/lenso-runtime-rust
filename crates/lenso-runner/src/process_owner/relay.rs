use super::wire;
use serde_json::{Value, json};
use std::{
    process::{ChildStdin, ChildStdout},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct Relay {
    input: mpsc::SyncSender<Value>,
    stop: Arc<Mutex<Option<Value>>>,
    pub finished: Arc<AtomicBool>,
}

impl Relay {
    pub fn start(
        mut input: ChildStdin,
        mut output: ChildStdout,
        events: mpsc::SyncSender<wire::Event>,
        invalid: Arc<AtomicBool>,
    ) -> Self {
        let (send, receive) = mpsc::sync_channel(8);
        let stop = Arc::new(Mutex::new(None));
        let stopping = stop.clone();
        let writer_invalid = invalid.clone();
        thread::spawn(move || {
            loop {
                if let Some(message) = stopping.lock().expect("stop slot").take() {
                    let _ = wire::write(&mut input, &message);
                    break;
                }
                match receive.recv_timeout(Duration::from_millis(5)) {
                    Ok(message) => {
                        if wire::write(&mut input, &message).is_err() {
                            writer_invalid.store(true, Ordering::Release);
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        let finished = Arc::new(AtomicBool::new(false));
        let done = finished.clone();
        thread::spawn(move || {
            loop {
                match wire::read::<Value>(&mut output) {
                    Ok(Some(message)) => {
                        if events
                            .try_send(wire::Event::Application { message })
                            .is_err()
                        {
                            invalid.store(true, Ordering::Release);
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(_) => {
                        invalid.store(true, Ordering::Release);
                        break;
                    }
                }
            }
            done.store(true, Ordering::Release);
        });
        Self {
            input: send,
            stop,
            finished,
        }
    }

    pub fn send(&self, message: Value) -> bool {
        self.input.try_send(message).is_ok()
    }

    pub fn stop(&self, message: Option<Value>) {
        let mut slot = self.stop.lock().expect("stop slot");
        if slot.is_none() {
            *slot = Some(message.unwrap_or_else(|| json!({"version":1,"id":0,"op":"stop"})));
        }
    }
}
