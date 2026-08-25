use std::{cell::Cell, collections::BTreeMap, rc::Rc};

use crate::{
    AppGenerationTransitionSpec, CanonicalDocument, ControlPlaneError, ReplacementMode,
    ResolvedGeneration, StateCompatibilityReceipt,
};

/// Host implementation which stages and retires complete Kernel App Generations.
pub trait GenerationRuntime: std::fmt::Debug {
    /// Opaque generation-owned Driver, Adapter catalog, Kernel, and host resources.
    type Handle: std::fmt::Debug;
    /// Cloneable invocation target retained by one route lease.
    type Route: Clone + std::fmt::Debug;

    /// Stages all resources and proves the Ready Gate before the timeout.
    fn stage<'a>(
        &'a mut self,
        generation: &'a ResolvedGeneration,
        ready_timeout_nanos: u64,
    ) -> futures::future::LocalBoxFuture<'a, Result<Self::Handle, ControlPlaneError>>;

    /// Stops new admission, drains, and releases generation-owned resources.
    fn shutdown(
        &mut self,
        handle: Self::Handle,
        drain_timeout_nanos: u64,
    ) -> futures::future::LocalBoxFuture<'_, Result<(), ControlPlaneError>>;

    /// Returns a terminal failure reported by a staged Generation, when one exists.
    ///
    /// A healthy or still-running Generation returns `None`. The Supervisor owns
    /// the policy decision which records the failure and may switch routes.
    fn terminal_failure(&self, handle: &Self::Handle) -> Option<ControlPlaneError>;

    /// Projects the exact staged Generation into its Host routing target.
    fn route(&self, handle: &Self::Handle) -> Self::Route;
}

/// Operator-visible lifecycle state of one staged Generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GenerationStatus {
    Active,
    Draining,
    RollbackStandby,
}

/// Route-pinned authority retained by one complete product operation or Turn.
#[derive(Debug)]
pub struct GenerationLease {
    generation_spec_digest: String,
    routing_epoch: u64,
    leases: Rc<Cell<usize>>,
}

impl GenerationLease {
    /// Returns the exact immutable Generation selected at route admission.
    pub fn generation_spec_digest(&self) -> &str {
        &self.generation_spec_digest
    }

    /// Returns the fencing epoch committed with this route.
    pub const fn routing_epoch(&self) -> u64 {
        self.routing_epoch
    }
}

impl Drop for GenerationLease {
    fn drop(&mut self) {
        self.leases.set(self.leases.get().saturating_sub(1));
    }
}

#[derive(Debug)]
struct GenerationSlot<H> {
    handle: H,
    generation: ResolvedGeneration,
    leases: Rc<Cell<usize>>,
    status: GenerationStatus,
}

/// Result of an atomic generation route switch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransitionOutcome {
    pub active_generation_spec_digest: String,
    pub routing_epoch: u64,
    pub draining_generation_spec_digest: Option<String>,
}

/// Single authority for staging, fenced route switching, leases, rollback, and retirement.
#[derive(Debug)]
pub struct GenerationSupervisor<R: GenerationRuntime> {
    app_id: String,
    runtime: R,
    routing_epoch: u64,
    active: Option<String>,
    slots: BTreeMap<String, GenerationSlot<R::Handle>>,
}

impl<R: GenerationRuntime> GenerationSupervisor<R> {
    /// Creates an empty Supervisor for one exact App identity.
    pub fn new(app_id: impl Into<String>, runtime: R) -> Self {
        Self {
            app_id: app_id.into(),
            runtime,
            routing_epoch: 0,
            active: None,
            slots: BTreeMap::new(),
        }
    }

    /// Stages and atomically switches one exact transition without mutating a live Plan.
    pub async fn transition(
        &mut self,
        transition: &CanonicalDocument<AppGenerationTransitionSpec>,
        candidate: &ResolvedGeneration,
    ) -> Result<TransitionOutcome, ControlPlaneError> {
        self.transition_with_receipts(transition, candidate, &BTreeMap::new())
            .await
    }

    /// Stages and switches after validating every exact state-compatibility receipt.
    pub async fn transition_with_receipts(
        &mut self,
        transition: &CanonicalDocument<AppGenerationTransitionSpec>,
        candidate: &ResolvedGeneration,
        receipts: &BTreeMap<String, CanonicalDocument<StateCompatibilityReceipt>>,
    ) -> Result<TransitionOutcome, ControlPlaneError> {
        let edge = transition.value();
        validate_edge(&self.app_id, self.active.as_deref(), edge, candidate)?;
        let previous = self
            .active
            .as_ref()
            .and_then(|digest| self.slots.get(digest))
            .map(|slot| &slot.generation);
        validate_state_compatibility(&self.app_id, previous, candidate, edge, receipts)?;
        let ready_timeout = parse_nanos(&edge.rollout_policy.ready_timeout_nanos, "ready timeout")?;
        let drain_timeout = parse_nanos(&edge.rollout_policy.drain_timeout_nanos, "drain timeout")?;

        if edge.replacement_mode == ReplacementMode::Maintenance {
            if edge.rollout_policy.rollback_window_nanos != "0"
                || edge.rollout_policy.automatic_rollback_on_generation_failure
            {
                return rejected("maintenance replacement cannot retain automatic rollback");
            }
            self.stop_active(drain_timeout).await?;
        }

        let candidate_digest = candidate.spec.digest().to_owned();
        if self.slots.contains_key(&candidate_digest) {
            return rejected("candidate Generation is already staged");
        }
        let handle = self.runtime.stage(candidate, ready_timeout).await?;
        let previous = self.active.replace(candidate_digest.clone());
        self.routing_epoch = self.routing_epoch.checked_add(1).ok_or_else(|| {
            ControlPlaneError::TransitionRejected {
                detail: "routing epoch exhausted".to_owned(),
            }
        })?;
        self.slots.insert(
            candidate_digest.clone(),
            GenerationSlot {
                handle,
                generation: candidate.clone(),
                leases: Rc::new(Cell::new(0)),
                status: GenerationStatus::Active,
            },
        );
        if let Some(previous) = &previous {
            let previous_slot = self
                .slots
                .get_mut(previous)
                .expect("active Generation must have one staged slot");
            previous_slot.status = if edge.rollout_policy.rollback_window_nanos == "0" {
                GenerationStatus::Draining
            } else {
                GenerationStatus::RollbackStandby
            };
        }
        Ok(TransitionOutcome {
            active_generation_spec_digest: candidate_digest,
            routing_epoch: self.routing_epoch,
            draining_generation_spec_digest: previous,
        })
    }

    /// Pins the current active Generation and routing epoch for one complete operation.
    pub fn lease(&self) -> Result<GenerationLease, ControlPlaneError> {
        let digest = self
            .active
            .as_ref()
            .ok_or_else(|| ControlPlaneError::TransitionRejected {
                detail: "no active App Generation".to_owned(),
            })?;
        let slot = self
            .slots
            .get(digest)
            .expect("active Generation must have one staged slot");
        slot.leases
            .set(slot.leases.get().checked_add(1).ok_or_else(|| {
                ControlPlaneError::TransitionRejected {
                    detail: "Generation lease count exhausted".to_owned(),
                }
            })?);
        Ok(GenerationLease {
            generation_spec_digest: digest.clone(),
            routing_epoch: self.routing_epoch,
            leases: slot.leases.clone(),
        })
    }

    /// Rolls the route back to an explicitly retained standby Generation.
    pub fn rollback(
        &mut self,
        standby_digest: &str,
    ) -> Result<TransitionOutcome, ControlPlaneError> {
        let standby = self.slots.get(standby_digest).ok_or_else(|| {
            ControlPlaneError::TransitionRejected {
                detail: format!("Generation `{standby_digest}` is not retained"),
            }
        })?;
        if standby.status != GenerationStatus::RollbackStandby {
            return rejected("rollback target is not in rollback standby");
        }
        let failed = self.active.replace(standby_digest.to_owned());
        self.routing_epoch = self.routing_epoch.checked_add(1).ok_or_else(|| {
            ControlPlaneError::TransitionRejected {
                detail: "routing epoch exhausted".to_owned(),
            }
        })?;
        self.slots
            .get_mut(standby_digest)
            .expect("standby slot was validated")
            .status = GenerationStatus::Active;
        if let Some(failed) = &failed {
            self.slots
                .get_mut(failed)
                .expect("active slot must exist")
                .status = GenerationStatus::Draining;
        }
        Ok(TransitionOutcome {
            active_generation_spec_digest: standby_digest.to_owned(),
            routing_epoch: self.routing_epoch,
            draining_generation_spec_digest: failed,
        })
    }

    /// Ends a rollback window and marks one standby Generation ready to drain.
    pub fn close_rollback_window(&mut self, digest: &str) -> Result<(), ControlPlaneError> {
        let slot =
            self.slots
                .get_mut(digest)
                .ok_or_else(|| ControlPlaneError::TransitionRejected {
                    detail: format!("unknown Generation `{digest}`"),
                })?;
        if slot.status != GenerationStatus::RollbackStandby {
            return rejected("Generation is not in rollback standby");
        }
        slot.status = GenerationStatus::Draining;
        Ok(())
    }

    /// Retires one drained Generation only after all route leases are released.
    pub async fn retire(
        &mut self,
        digest: &str,
        drain_timeout_nanos: u64,
    ) -> Result<(), ControlPlaneError> {
        let slot = self
            .slots
            .get(digest)
            .ok_or_else(|| ControlPlaneError::TransitionRejected {
                detail: format!("unknown Generation `{digest}`"),
            })?;
        if slot.status != GenerationStatus::Draining || slot.leases.get() != 0 {
            return rejected("Generation is not drained or still has active leases");
        }
        let slot = self.slots.remove(digest).expect("slot was validated");
        self.runtime
            .shutdown(slot.handle, drain_timeout_nanos)
            .await
    }

    /// Returns current lifecycle state and lease count for operator UX.
    pub fn generations(&self) -> Vec<(String, GenerationStatus, usize)> {
        self.slots
            .iter()
            .map(|(digest, slot)| (digest.clone(), slot.status, slot.leases.get()))
            .collect()
    }

    async fn stop_active(&mut self, drain_timeout_nanos: u64) -> Result<(), ControlPlaneError> {
        let Some(active) = self.active.take() else {
            return Ok(());
        };
        let slot = self
            .slots
            .get(&active)
            .expect("active Generation must have one staged slot");
        if slot.leases.get() != 0 {
            self.active = Some(active);
            return rejected("maintenance replacement cannot stop a leased Generation");
        }
        let slot = self.slots.remove(&active).expect("slot was validated");
        self.runtime
            .shutdown(slot.handle, drain_timeout_nanos)
            .await
    }
}

pub(crate) fn validate_state_compatibility(
    app_id: &str,
    previous: Option<&ResolvedGeneration>,
    candidate: &ResolvedGeneration,
    edge: &AppGenerationTransitionSpec,
    receipts: &BTreeMap<String, CanonicalDocument<StateCompatibilityReceipt>>,
) -> Result<(), ControlPlaneError> {
    if edge.replacement_mode != ReplacementMode::Overlap {
        if !edge.state_compatibility_receipt_digests.is_empty() || !receipts.is_empty() {
            return rejected("compatibility receipts are accepted only for overlap replacement");
        }
        return Ok(());
    }
    let previous = previous.expect("overlap edge validation requires a predecessor");
    let mut instance_keys = previous
        .stateful_instances
        .keys()
        .chain(candidate.stateful_instances.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let mut required_digests = std::collections::BTreeSet::new();
    for instance_key in std::mem::take(&mut instance_keys) {
        let old = previous.stateful_instances.get(&instance_key);
        let new = candidate.stateful_instances.get(&instance_key);
        if old == new {
            continue;
        }
        let (Some(old), Some(new)) = (old, new) else {
            return rejected(format!(
                "stateful Instance `{instance_key}` cannot be added or removed during overlap"
            ));
        };
        let document = receipts
            .values()
            .find(|document| document.value().module_instance_key == instance_key)
            .ok_or_else(|| ControlPlaneError::TransitionRejected {
                detail: format!(
                    "changed stateful Instance `{instance_key}` has no compatibility receipt"
                ),
            })?;
        let receipt = document.value();
        if receipt.schema_version != 1
            || receipt.app_id != app_id
            || receipt.old_runtime_identity != old.runtime_identity
            || receipt.new_runtime_identity != new.runtime_identity
            || receipt.state_schema_id != old.state_schema_id
            || receipt.state_schema_id != new.state_schema_id
            || receipt.old_state_schema_digest != old.state_schema_digest
            || receipt.new_state_schema_digest != new.state_schema_digest
            || !receipt.compatibility.concurrent_read
            || !receipt.compatibility.concurrent_write
            || ((edge.rollout_policy.rollback_window_nanos != "0"
                || edge.rollout_policy.automatic_rollback_on_generation_failure)
                && !receipt.compatibility.old_code_reads_new_writes)
        {
            return rejected(format!(
                "compatibility receipt for `{instance_key}` does not close over this transition"
            ));
        }
        required_digests.insert(document.digest().to_owned());
    }
    let declared = edge
        .state_compatibility_receipt_digests
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let supplied = receipts
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if declared.len() != edge.state_compatibility_receipt_digests.len()
        || declared != required_digests
        || supplied != required_digests
        || receipts
            .iter()
            .any(|(digest, document)| digest != document.digest())
    {
        return rejected("Transition compatibility receipt digest closure is not exact");
    }
    Ok(())
}

pub(crate) fn validate_edge(
    app_id: &str,
    active: Option<&str>,
    edge: &AppGenerationTransitionSpec,
    candidate: &ResolvedGeneration,
) -> Result<(), ControlPlaneError> {
    if edge.schema_version != 1
        || edge.app_id != app_id
        || candidate.spec.value().app_id != app_id
        || edge.to_generation_spec_digest != candidate.spec.digest()
    {
        return rejected("transition authority does not close over the candidate Generation");
    }
    if edge.from_generation_spec_digest.as_deref() != active {
        return rejected("transition predecessor does not match the fenced active Generation");
    }
    match edge.replacement_mode {
        ReplacementMode::Initial if active.is_none() => {
            if !edge.state_compatibility_receipt_digests.is_empty()
                || edge.rollout_policy.automatic_rollback_on_generation_failure
            {
                return rejected(
                    "initial transition cannot use compatibility or rollback authority",
                );
            }
        }
        ReplacementMode::Initial => return rejected("initial transition requires no predecessor"),
        ReplacementMode::Overlap | ReplacementMode::Maintenance if active.is_none() => {
            return rejected("replacement transition requires one active predecessor");
        }
        ReplacementMode::Overlap | ReplacementMode::Maintenance => {}
    }
    Ok(())
}

fn parse_nanos(value: &str, kind: &str) -> Result<u64, ControlPlaneError> {
    value
        .parse::<u64>()
        .map_err(|_| ControlPlaneError::TransitionRejected {
            detail: format!("{kind} is not a bounded unsigned integer"),
        })
}

fn rejected<T>(detail: impl Into<String>) -> Result<T, ControlPlaneError> {
    Err(ControlPlaneError::TransitionRejected {
        detail: detail.into(),
    })
}
