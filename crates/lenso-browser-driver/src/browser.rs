use std::{
    cell::{Cell, RefCell},
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
use wasm_bindgen::{JsCast, closure::Closure};
use wasm_bindgen_futures::spawn_local;
use web_sys::window;

#[derive(Debug)]
struct BrowserState {
    started_at_ms: f64,
    shutdown_requested: Cell<bool>,
    jitter_state: Cell<u64>,
}

/// Runtime Driver backed by the browser/JavaScript local event loop.
#[derive(Clone, Debug)]
pub struct BrowserDriver {
    state: Rc<BrowserState>,
}

struct BrowserTimer {
    driver: BrowserDriver,
    deadline: Duration,
    wait_for_turn: bool,
    receiver: Option<oneshot::Receiver<()>>,
    timer_id: Option<i32>,
    callback: Option<Closure<dyn FnMut()>>,
}

impl Unpin for BrowserTimer {}

impl BrowserTimer {
    fn new(driver: BrowserDriver, deadline: Duration) -> Self {
        Self {
            driver,
            deadline,
            wait_for_turn: false,
            receiver: None,
            timer_id: None,
            callback: None,
        }
    }

    fn next_turn(driver: BrowserDriver) -> Self {
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
        let timer_id = window()
            .expect("the browser Driver requires a Window host")
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.as_ref().unchecked_ref(),
                milliseconds,
            )
            .expect("the browser host should accept a timer callback");
        self.receiver = Some(receiver);
        self.timer_id = Some(timer_id);
        self.callback = Some(callback);
    }

    fn cancel_timer(&mut self) {
        if let Some(timer_id) = self.timer_id.take()
            && let Some(window) = window()
        {
            window.clear_timeout_with_handle(timer_id);
        }
        self.receiver.take();
        self.callback.take();
    }
}

impl Future for BrowserTimer {
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
                    .expect("a browser timer is armed before it is polled"),
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

impl Drop for BrowserTimer {
    fn drop(&mut self) {
        self.cancel_timer();
    }
}

impl BrowserDriver {
    /// Creates a Driver using the host's monotonic `performance.now()` clock.
    pub fn new() -> Self {
        let started_at_ms = performance_now();
        Self {
            state: Rc::new(BrowserState {
                started_at_ms,
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
    }
}

impl Default for BrowserDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeDriver for BrowserDriver {
    fn now(&self) -> Duration {
        let elapsed_ms = (performance_now() - self.state.started_at_ms).max(0.0);
        Duration::from_nanos(duration_nanos(elapsed_ms))
    }

    fn sleep_until(&self, deadline: Duration) -> LocalBoxFuture<'static, ()> {
        if deadline <= self.now() {
            return Box::pin(futures::future::ready(()));
        }
        Box::pin(BrowserTimer::new(self.clone(), deadline))
    }

    fn yield_now(&self) -> LocalBoxFuture<'static, ()> {
        Box::pin(BrowserTimer::next_turn(self.clone()))
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
        let (abort, registration) = AbortHandle::new_pair();
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
            let _ = completed.send(outcome);
        });
        Ok(DriverTask::new(abort, completion))
    }

    fn shutdown_requested(&self) -> bool {
        self.state.shutdown_requested.get()
    }
}

fn performance_now() -> f64 {
    window()
        .expect("the browser Driver requires a Window host")
        .performance()
        .expect("the browser Driver requires the Performance API")
        .now()
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
