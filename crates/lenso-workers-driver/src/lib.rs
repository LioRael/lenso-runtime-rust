//! Workers event-loop Driver. Pair with `@lenso/workers-runtime` in the JavaScript Host.
//! The Host owns event admission, I/O fencing, and generation retirement.
#![cfg(all(target_arch = "wasm32", target_os = "unknown"))]

use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    future::Future,
    panic::AssertUnwindSafe,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
    time::Duration,
};

use futures::{
    channel::oneshot,
    future::{AbortHandle, Abortable, FutureExt, LocalBoxFuture},
    task::SpawnError,
};
use lenso_kernel::{DriverTask, LocalTask, RuntimeDriver, TaskOutcome};
use wasm_bindgen::closure::Closure;
use wasm_bindgen_futures::spawn_local;
#[wasm_bindgen::prelude::wasm_bindgen(raw_module = "@lenso/workers-runtime/clock")]
extern "C" {
    fn clock(operation: u32, callback: &wasm_bindgen::JsValue, value: i32) -> f64;
}

fn host_timer(callback: &wasm_bindgen::JsValue, delay: i32) -> i32 {
    // The host returns an integer timer ID in the signed 32-bit range.
    #[allow(clippy::cast_possible_truncation)]
    let id = clock(1, callback, delay) as i32;
    id
}

fn host_clear(id: i32) {
    clock(2, &wasm_bindgen::JsValue::NULL, id);
}

#[derive(Debug)]
struct WorkersState {
    started_at_ms: f64,
    shutdown_requested: Cell<bool>,
    jitter_state: Cell<u64>,
    next_task: Cell<u64>,
    tasks: RefCell<BTreeMap<u64, AbortHandle>>,
    event: futures::task::AtomicWaker,
}

/// Runtime Driver backed by the Workers/JavaScript local event loop.
#[derive(Clone, Debug)]
pub struct WorkersDriver {
    state: Rc<WorkersState>,
}

struct WorkersTimer {
    driver: WorkersDriver,
    deadline: Duration,
    wait_for_turn: bool,
    receiver: Option<oneshot::Receiver<()>>,
    timer_id: Option<i32>,
    callback: Option<Closure<dyn FnMut()>>,
}

impl Unpin for WorkersTimer {}

impl WorkersTimer {
    fn new(driver: WorkersDriver, deadline: Duration) -> Self {
        Self {
            driver,
            deadline,
            wait_for_turn: false,
            receiver: None,
            timer_id: None,
            callback: None,
        }
    }

    fn next_turn(driver: WorkersDriver) -> Self {
        Self {
            driver,
            deadline: Duration::ZERO,
            wait_for_turn: true,
            receiver: None,
            timer_id: None,
            callback: None,
        }
    }

    fn arm(&mut self) {
        let delay = if self.wait_for_turn {
            Duration::ZERO
        } else {
            self.deadline.saturating_sub(self.driver.now())
        };
        let milliseconds =
            i32::try_from(delay.as_millis().min(2_147_483_647_u128)).unwrap_or(i32::MAX);
        let (wakeup, receiver) = oneshot::channel();
        let wakeup = Rc::new(RefCell::new(Some(wakeup)));
        let callback_wakeup = wakeup.clone();
        let callback = Closure::new(move || {
            if let Some(wakeup) = callback_wakeup.borrow_mut().take() {
                let _ = wakeup.send(());
            }
        });
        let timer_id = host_timer(callback.as_ref(), milliseconds);
        self.receiver = Some(receiver);
        self.timer_id = Some(timer_id);
        self.callback = Some(callback);
    }

    fn cancel_timer(&mut self) {
        if let Some(timer_id) = self.timer_id.take() {
            host_clear(timer_id);
        }
        self.receiver.take();
        self.callback.take();
    }
}

impl Future for WorkersTimer {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            if !self.wait_for_turn && self.deadline <= self.driver.now() {
                self.cancel_timer();
                return Poll::Ready(());
            }
            if self.receiver.is_none() {
                self.arm();
            }
            let result = Pin::new(
                self.receiver
                    .as_mut()
                    .expect("a Workers timer is armed before it is polled"),
            )
            .poll(context);
            match result {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(_) => {
                    self.timer_id = None;
                    self.callback.take();
                    self.receiver = None;
                    if self.wait_for_turn {
                        return Poll::Ready(());
                    }
                }
            }
        }
    }
}

impl Drop for WorkersTimer {
    fn drop(&mut self) {
        self.cancel_timer();
    }
}

impl WorkersDriver {
    /// Creates a Driver using the host's monotonic `performance.now()` clock.
    pub fn new() -> Self {
        let started_at_ms = performance_now();
        Self {
            state: Rc::new(WorkersState {
                started_at_ms,
                next_task: Cell::new(0),
                tasks: RefCell::new(BTreeMap::new()),
                event: futures::task::AtomicWaker::new(),
                shutdown_requested: Cell::new(false),
                jitter_state: Cell::new(started_at_ms.to_bits() ^ 0x9e37_79b9_7f4a_7c15),
            }),
        }
    }

    /// Schedules a root Kernel task on the JavaScript event loop.
    pub fn spawn_root(&self, task: LocalTask) -> Result<DriverTask, SpawnError> {
        self.spawn_local(task)
    }

    /// Requests cooperative shutdown from the embedding JavaScript host.
    pub fn request_shutdown(&self) {
        self.state.shutdown_requested.set(true);
        for task in self.state.tasks.borrow().values() {
            task.abort();
        }
        self.state.event.wake();
    }
}

impl Default for WorkersDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeDriver for WorkersDriver {
    fn now(&self) -> Duration {
        let elapsed_ms = (performance_now() - self.state.started_at_ms).max(0.0);
        Duration::from_nanos(duration_nanos(elapsed_ms))
    }

    fn sleep_until(&self, deadline: Duration) -> LocalBoxFuture<'static, ()> {
        if deadline <= self.now() {
            return Box::pin(futures::future::ready(()));
        }
        Box::pin(WorkersTimer::new(self.clone(), deadline))
    }

    fn yield_now(&self) -> LocalBoxFuture<'static, ()> {
        Box::pin(WorkersTimer::next_turn(self.clone()))
    }

    fn wait_for_runtime_event(&self, deadline: Duration) -> LocalBoxFuture<'static, ()> {
        let state = self.state.clone();
        let shutdown = Box::pin(futures::future::poll_fn(move |cx| {
            state.event.register(cx.waker());
            if state.shutdown_requested.get() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }));
        let timer = self.sleep_until(deadline);
        Box::pin(async move {
            futures::future::select(shutdown, timer).await;
        })
    }

    fn jitter(&self, maximum: Duration) -> Duration {
        if maximum.is_zero() {
            return Duration::ZERO;
        }
        let next = self
            .state
            .jitter_state
            .get()
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state.jitter_state.set(next);
        let maximum_nanos = maximum.as_nanos().min(u128::from(u64::MAX));
        let jitter_nanos = u128::from(next) % maximum_nanos.saturating_add(1);
        Duration::from_nanos(u64::try_from(jitter_nanos).unwrap_or(u64::MAX))
    }

    fn spawn_local(&self, task: LocalTask) -> Result<DriverTask, SpawnError> {
        if self.state.shutdown_requested.get() || self.state.tasks.borrow().len() >= 128 {
            return Err(SpawnError::shutdown());
        }
        let id = self.state.next_task.get();
        let next = id.checked_add(1).ok_or_else(SpawnError::shutdown)?;
        self.state.next_task.set(next);
        let (abort, registration) = AbortHandle::new_pair();
        self.state.tasks.borrow_mut().insert(id, abort.clone());
        let state = self.state.clone();
        let (completed, completion) = oneshot::channel();
        spawn_local(async move {
            let outcome = match AssertUnwindSafe(Abortable::new(task, registration))
                .catch_unwind()
                .await
            {
                Ok(Ok(())) => TaskOutcome::Completed,
                Ok(Err(_)) => TaskOutcome::Cancelled,
                Err(_) => TaskOutcome::Failed,
            };
            state.tasks.borrow_mut().remove(&id);
            let _ = completed.send(outcome);
        });
        Ok(DriverTask::new(abort, completion))
    }

    fn shutdown_requested(&self) -> bool {
        self.state.shutdown_requested.get()
    }
}

fn performance_now() -> f64 {
    clock(0, &wasm_bindgen::JsValue::NULL, 0)
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn duration_nanos(milliseconds: f64) -> u64 {
    if !milliseconds.is_finite() || milliseconds <= 0.0 {
        return 0;
    }
    let nanos = milliseconds * 1_000_000.0;
    if nanos >= u64::MAX as f64 {
        u64::MAX
    } else {
        nanos.ceil() as u64
    }
}
