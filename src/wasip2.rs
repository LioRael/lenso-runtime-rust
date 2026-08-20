use std::{
    cell::{Cell, RefCell},
    future::Future,
    panic::AssertUnwindSafe,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use futures::{
    channel::oneshot,
    executor::{LocalPool, LocalSpawner},
    future::{AbortHandle, Abortable, FutureExt, LocalBoxFuture},
    task::{LocalSpawnExt, SpawnError},
};
use lenso_kernel::{DriverTask, LocalTask, RuntimeDriver, TaskOutcome};

#[derive(Debug)]
struct WasiTimerEntry {
    id: u64,
    deadline: Duration,
    wakeup: oneshot::Sender<()>,
}

#[derive(Debug)]
struct WasiState {
    started_at: Instant,
    shutdown_requested: Cell<bool>,
    jitter_state: Cell<u64>,
    next_timer_id: Cell<u64>,
    pool: RefCell<Option<LocalPool>>,
    spawner: LocalSpawner,
    timers: RefCell<Vec<WasiTimerEntry>>,
}

/// WASI Preview 2 Driver using the host monotonic clock and a host-pumped lane.
///
/// The embedding component calls [`WasiDriver::pump`] after its WASI poller
/// wakes for [`WasiDriver::next_timer`]. No thread, Tokio runtime, process
/// signal, or ambient filesystem is required by this Driver.
#[derive(Clone, Debug)]
pub struct WasiDriver {
    state: Rc<WasiState>,
}

struct WasiTimer {
    state: Rc<WasiState>,
    deadline: Duration,
    timer_id: Option<u64>,
    receiver: Option<oneshot::Receiver<()>>,
}

impl Unpin for WasiTimer {}

impl WasiTimer {
    fn new(state: Rc<WasiState>, deadline: Duration) -> Self {
        Self {
            state,
            deadline,
            timer_id: None,
            receiver: None,
        }
    }

    fn arm(&mut self) {
        let timer_id = self.state.next_timer_id.get();
        self.state.next_timer_id.set(timer_id.saturating_add(1));
        let (wakeup, receiver) = oneshot::channel();
        self.state.timers.borrow_mut().push(WasiTimerEntry {
            id: timer_id,
            deadline: self.deadline,
            wakeup,
        });
        self.timer_id = Some(timer_id);
        self.receiver = Some(receiver);
    }

    fn remove_timer(&mut self) {
        if let Some(timer_id) = self.timer_id.take() {
            self.state
                .timers
                .borrow_mut()
                .retain(|entry| entry.id != timer_id);
        }
        self.receiver.take();
    }
}

impl Future for WasiTimer {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            if self.deadline <= self.state.started_at.elapsed() {
                self.remove_timer();
                return Poll::Ready(());
            }
            if self.receiver.is_none() {
                self.arm();
            }
            let result = Pin::new(
                self.receiver
                    .as_mut()
                    .expect("a WASIp2 timer is armed before it is polled"),
            )
            .poll(context);
            match result {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(_) => {
                    self.timer_id = None;
                    self.receiver = None;
                }
            }
        }
    }
}

impl Drop for WasiTimer {
    fn drop(&mut self) {
        self.remove_timer();
    }
}

impl WasiDriver {
    /// Creates a Driver bound to the WASI monotonic clock.
    pub fn new() -> Self {
        let pool = LocalPool::new();
        let spawner = pool.spawner();
        let started_at = Instant::now();
        Self {
            state: Rc::new(WasiState {
                started_at,
                shutdown_requested: Cell::new(false),
                jitter_state: Cell::new(
                    u64::try_from(started_at.elapsed().as_nanos().min(u128::from(u64::MAX)))
                        .unwrap_or(u64::MAX)
                        ^ 0x9e37_79b9_7f4a_7c15,
                ),
                next_timer_id: Cell::new(0),
                pool: RefCell::new(Some(pool)),
                spawner,
                timers: RefCell::new(Vec::new()),
            }),
        }
    }

    /// Schedules one root task; the host advances it by calling [`Self::pump`].
    pub fn spawn_root(&self, task: LocalTask) -> Result<DriverTask, SpawnError> {
        self.spawn_local(task)
    }

    /// Runs ready local tasks and wakes timers whose monotonic deadline passed.
    pub fn pump(&self) {
        let now = self.now();
        let mut timers = self.state.timers.borrow_mut();
        let mut pending = Vec::with_capacity(timers.len());
        for entry in timers.drain(..) {
            if entry.deadline <= now {
                let _ = entry.wakeup.send(());
            } else {
                pending.push(entry);
            }
        }
        *timers = pending;
        drop(timers);

        let mut pool = self
            .state
            .pool
            .borrow_mut()
            .take()
            .expect("WASIp2 Driver cannot pump recursively");
        pool.run_until_stalled();
        self.state.pool.replace(Some(pool));
    }

    /// Returns the next timer deadline for the host's WASI poller.
    pub fn next_timer(&self) -> Option<Duration> {
        self.state
            .timers
            .borrow()
            .iter()
            .map(|entry| entry.deadline)
            .min()
    }

    /// Requests cooperative shutdown from the embedding WASI host.
    pub fn request_shutdown(&self) {
        self.state.shutdown_requested.set(true);
    }
}

impl Default for WasiDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeDriver for WasiDriver {
    fn now(&self) -> Duration {
        self.state.started_at.elapsed()
    }

    fn sleep_until(&self, deadline: Duration) -> LocalBoxFuture<'static, ()> {
        if deadline <= self.now() {
            return Box::pin(futures::future::ready(()));
        }
        Box::pin(WasiTimer::new(self.state.clone(), deadline))
    }

    fn yield_now(&self) -> LocalBoxFuture<'static, ()> {
        let mut yielded = false;
        Box::pin(futures::future::poll_fn(move |context| {
            if yielded {
                Poll::Ready(())
            } else {
                yielded = true;
                context.waker().wake_by_ref();
                Poll::Pending
            }
        }))
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
        self.state.spawner.spawn_local(async move {
            let outcome = match AssertUnwindSafe(Abortable::new(task, registration))
                .catch_unwind()
                .await
            {
                Ok(Ok(())) => TaskOutcome::Completed,
                Ok(Err(_)) => TaskOutcome::Cancelled,
                Err(_) => TaskOutcome::Failed,
            };
            let _ = completed.send(outcome);
        })?;
        Ok(DriverTask::new(abort, completion))
    }

    fn shutdown_requested(&self) -> bool {
        self.state.shutdown_requested.get()
    }
}
