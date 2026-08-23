use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

use super::ReplicatedRunnerError;

#[derive(Debug, Default)]
pub(super) struct ReplicatedTerminalState {
    failed: AtomicBool,
    stopping: AtomicBool,
    failure: Mutex<Option<ReplicatedRunnerError>>,
    changed: tokio::sync::Notify,
}

impl ReplicatedTerminalState {
    pub(super) fn fail(&self, failure: ReplicatedRunnerError) {
        let mut current = self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.is_some() {
            return;
        }
        *current = Some(failure);
        self.failed.store(true, Ordering::Release);
        drop(current);
        self.changed.notify_waiters();
    }

    pub(super) fn failure(&self) -> Option<ReplicatedRunnerError> {
        if !self.failed.load(Ordering::Acquire) {
            return None;
        }
        self.failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(super) fn is_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    pub(super) fn begin_shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
    }

    pub(super) fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    pub(super) async fn wait(&self) -> ReplicatedRunnerError {
        loop {
            let changed = self.changed.notified();
            if let Some(failure) = self.failure() {
                return failure;
            }
            changed.await;
        }
    }
}
