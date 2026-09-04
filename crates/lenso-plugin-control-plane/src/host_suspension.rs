use tokio::time::Instant;

use super::{
    ControlPlaneError, ControlStateStore, DurableControlState, DurableGenerationSupervisor,
    GenerationRuntime,
};

impl<R: GenerationRuntime, S: ControlStateStore> DurableGenerationSupervisor<R, S> {
    /// Controller-only terminal operation. Admission must already be closed; the
    /// controller must exit on either outcome, never resume a partially stopped graph.
    pub(crate) async fn drain_and_suspend_host(
        &mut self,
        deadline: Instant,
    ) -> Result<DurableControlState, ControlPlaneError> {
        let drain = async {
            while self.slots.values().any(|slot| slot.leases.get() != 0) {
                self.lease_released.notified().await;
            }
        };
        tokio::time::timeout_at(deadline, drain)
            .await
            .map_err(|_| unconfirmed("waiting for active leases"))?;

        let mut failures = Vec::new();
        let digests = self.slots.keys().cloned().collect::<Vec<_>>();
        for digest in digests {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(unconfirmed("stopping Generations"));
            }
            let slot = self.slots.remove(&digest).expect("closed live slot set");
            // An already failed Generation must never become a clean suspension.
            if let Some(error) = self.runtime.terminal_failure(&slot.handle) {
                failures.push(format!("Generation `{digest}` failed: {error}"));
            }
            let nanos = u64::try_from(remaining.as_nanos()).unwrap_or(u64::MAX);
            match tokio::time::timeout_at(deadline, self.runtime.shutdown(slot.handle, nanos)).await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failures.push(format!("Generation `{digest}` cleanup: {error}")),
                Err(_) => return Err(unconfirmed("stopping Generations")),
            }
        }
        if !failures.is_empty() {
            return Err(ControlPlaneError::HostFailure {
                detail: format!(
                    "Host suspension failed; no clean suspension recorded: {}",
                    failures.join("; ")
                ),
            });
        }
        if Instant::now() >= deadline {
            return Err(unconfirmed("before persisting suspension"));
        }
        let mut next = self.state.clone();
        next.host_suspended = true;
        self.commit(next)?;
        Ok(self.state.clone())
    }
}

fn unconfirmed(phase: &str) -> ControlPlaneError {
    ControlPlaneError::HostFailure {
        detail: format!(
            "Host suspension deadline exceeded while {phase}; execution termination is unconfirmed; native process ownership must settle execution before recovery"
        ),
    }
}
