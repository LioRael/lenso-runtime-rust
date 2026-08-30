use std::{cell::Cell, collections::BTreeMap, rc::Rc};

use crate::{
    ActivationDirection, AppGenerationTransitionSpec, CanonicalDocument, ControlHealth,
    ControlLifecycle, ControlPlaneError, ControlStateStore, DurableControlState,
    GenerationControlRecord, GenerationRuntime, ReplacementMode, ResolvedGeneration,
    RetirementReason, StateCompatibilityReceipt,
    supervisor::{validate_edge, validate_state_compatibility},
};

#[derive(Debug)]
struct LiveSlot<H> {
    generation: ResolvedGeneration,
    handle: H,
    leases: Rc<Cell<usize>>,
}

/// Fenced, route-pinned authority for one complete product operation.
#[derive(Debug)]
pub struct DurableGenerationLease {
    generation_spec_digest: String,
    supervisor_epoch: u64,
    routing_epoch: u64,
    leases: Rc<Cell<usize>>,
}

/// Route target pinned to one immutable Generation for a complete work unit.
#[derive(Debug)]
pub struct DurableGenerationRoute<T: std::fmt::Debug> {
    target: T,
    lease: DurableGenerationLease,
}

impl<T: std::fmt::Debug> DurableGenerationRoute<T> {
    pub const fn target(&self) -> &T {
        &self.target
    }

    pub fn generation_spec_digest(&self) -> &str {
        self.lease.generation_spec_digest()
    }

    pub const fn supervisor_epoch(&self) -> u64 {
        self.lease.supervisor_epoch()
    }

    pub const fn routing_epoch(&self) -> u64 {
        self.lease.routing_epoch()
    }

    pub fn into_parts(self) -> (T, DurableGenerationLease) {
        (self.target, self.lease)
    }
}

impl DurableGenerationLease {
    pub fn generation_spec_digest(&self) -> &str {
        &self.generation_spec_digest
    }

    pub const fn supervisor_epoch(&self) -> u64 {
        self.supervisor_epoch
    }

    pub const fn routing_epoch(&self) -> u64 {
        self.routing_epoch
    }
}

impl Drop for DurableGenerationLease {
    fn drop(&mut self) {
        self.leases.set(self.leases.get().saturating_sub(1));
    }
}

/// Durable result of a forward route switch or rollback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableTransitionOutcome {
    pub active_generation_spec_digest: String,
    pub supervisor_epoch: u64,
    pub routing_epoch: u64,
    pub draining_generation_spec_digest: Option<String>,
    pub activation_direction: ActivationDirection,
}

/// One newly observed terminal Generation failure and its policy result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenerationFailureOutcome {
    pub generation_spec_digest: String,
    pub failure: ControlPlaneError,
    pub automatic_rollback: Option<DurableTransitionOutcome>,
}

/// Observable result of one automatic lifecycle-maintenance pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GenerationMaintenanceOutcome {
    Failed(GenerationFailureOutcome),
    Standby {
        generation_spec_digest: String,
    },
    Retired {
        generation_spec_digest: String,
        reason: RetirementReason,
    },
}

/// Fenced App Generation state machine with durable CAS authority and recovery.
#[derive(Debug)]
pub struct DurableGenerationSupervisor<R: GenerationRuntime, S: ControlStateStore> {
    app_id: String,
    runtime: R,
    store: S,
    state: DurableControlState,
    slots: BTreeMap<String, LiveSlot<R::Handle>>,
}

impl<R: GenerationRuntime, S: ControlStateStore> DurableGenerationSupervisor<R, S> {
    /// Opens a new or fully retired control state. Live state must use `recover`.
    pub fn open(
        app_id: impl Into<String>,
        runtime: R,
        store: S,
    ) -> Result<Self, ControlPlaneError> {
        let app_id = app_id.into();
        let state = store.load(&app_id)?;
        if state.generations.iter().any(|record| {
            matches!(
                record.lifecycle,
                ControlLifecycle::Staged
                    | ControlLifecycle::Ready
                    | ControlLifecycle::Active
                    | ControlLifecycle::Draining
                    | ControlLifecycle::Standby
            )
        }) {
            return rejected("live durable control state requires explicit recovery");
        }
        let mut next = state.clone();
        next.host_suspended = false;
        next.supervisor_epoch = next.supervisor_epoch.checked_add(1).ok_or_else(|| {
            ControlPlaneError::TransitionRejected {
                detail: "Supervisor epoch exhausted".to_owned(),
            }
        })?;
        let state = store.compare_and_swap(&app_id, state.revision, next)?;
        Ok(Self {
            app_id,
            runtime,
            store,
            state,
            slots: BTreeMap::new(),
        })
    }

    /// Opens a fresh Host incarnation after a proven clean suspension when the previous
    /// Generation cannot be restaged by the replacement Host build.
    ///
    /// The previous process must have committed `host_suspended` only after releasing every
    /// process-local Generation resource. Unclean or concurrent Host replacement fails closed.
    pub fn replace_suspended_host(
        app_id: impl Into<String>,
        runtime: R,
        store: S,
    ) -> Result<Self, ControlPlaneError> {
        let app_id = app_id.into();
        let state = store.load(&app_id)?;
        if !state.host_suspended {
            return rejected("Host replacement requires a clean durable suspension");
        }
        let mut next = state.clone();
        next.host_suspended = false;
        next.supervisor_epoch = next.supervisor_epoch.checked_add(1).ok_or_else(|| {
            ControlPlaneError::TransitionRejected {
                detail: "Supervisor epoch exhausted".to_owned(),
            }
        })?;
        if next.active_generation_spec_digest.take().is_some() {
            next.routing_epoch = next.routing_epoch.checked_add(1).ok_or_else(|| {
                ControlPlaneError::TransitionRejected {
                    detail: "Routing Epoch exhausted".to_owned(),
                }
            })?;
        }
        for record in &mut next.generations {
            if matches!(
                record.lifecycle,
                ControlLifecycle::Staged
                    | ControlLifecycle::Ready
                    | ControlLifecycle::Active
                    | ControlLifecycle::Draining
                    | ControlLifecycle::Standby
            ) {
                record.lifecycle = ControlLifecycle::Retired;
                record.drain_deadline_unix_nanos = None;
                record.retirement_reason = Some(RetirementReason::HostBuildReplaced);
            }
        }
        let state = store.compare_and_swap(&app_id, state.revision, next)?;
        Ok(Self {
            app_id,
            runtime,
            store,
            state,
            slots: BTreeMap::new(),
        })
    }

    /// Fences the previous Supervisor, reconciles interrupted lifecycle records, and restages
    /// only the durably active and standby Generations from exact supplied authority.
    #[allow(clippy::too_many_lines)]
    pub async fn recover(
        app_id: impl Into<String>,
        mut runtime: R,
        store: S,
        generations: &BTreeMap<String, ResolvedGeneration>,
        now_unix_nanos: u128,
    ) -> Result<Self, ControlPlaneError> {
        let app_id = app_id.into();
        let mut state = store.load(&app_id)?;
        state.host_suspended = false;
        state.supervisor_epoch = state.supervisor_epoch.checked_add(1).ok_or_else(|| {
            ControlPlaneError::TransitionRejected {
                detail: "Supervisor epoch exhausted".to_owned(),
            }
        })?;
        state.routing_epoch = state.routing_epoch.checked_add(1).ok_or_else(|| {
            ControlPlaneError::TransitionRejected {
                detail: "Routing Epoch exhausted".to_owned(),
            }
        })?;
        state = store.compare_and_swap(&app_id, state.revision, state)?;

        let mut slots = BTreeMap::new();
        macro_rules! recover_or_cleanup {
            ($result:expr) => {
                match $result {
                    Ok(value) => value,
                    Err(error) => {
                        shutdown_recovered_slots(&mut runtime, slots, &state).await;
                        return Err(error);
                    }
                }
            };
        }
        let mut failed_active: Option<(String, bool)> = None;
        let mut next = state.clone();
        for record in &mut next.generations {
            match record.lifecycle {
                ControlLifecycle::Active | ControlLifecycle::Standby => {
                    let generation = recover_or_cleanup!(
                        generations
                            .get(&record.generation_spec_digest)
                            .ok_or_else(|| ControlPlaneError::TransitionRejected {
                                detail: format!(
                                    "recovery lacks exact Generation `{}` authority",
                                    record.generation_spec_digest
                                ),
                            })
                    );
                    if generation.spec.digest() != record.generation_spec_digest
                        || generation.spec.value().app_id != app_id
                    {
                        recover_or_cleanup!(rejected::<()>(
                            "recovery Generation authority digest or App mismatch"
                        ));
                    }
                    if record.lifecycle == ControlLifecycle::Standby
                        && recover_or_cleanup!(deadline(record)) <= now_unix_nanos
                    {
                        record.lifecycle = ControlLifecycle::Retired;
                        record.retirement_reason = Some(RetirementReason::RollbackWindowExpired);
                        continue;
                    }
                    let timeout = recover_or_cleanup!(parse_nanos(
                        &record.ready_timeout_nanos,
                        "ready timeout"
                    ));
                    match runtime.stage(generation, timeout).await {
                        Ok(handle) => {
                            slots.insert(
                                record.generation_spec_digest.clone(),
                                LiveSlot {
                                    generation: generation.clone(),
                                    handle,
                                    leases: Rc::new(Cell::new(0)),
                                },
                            );
                        }
                        Err(error) => {
                            if next.active_generation_spec_digest.as_deref()
                                == Some(&record.generation_spec_digest)
                            {
                                failed_active = Some((
                                    record.transition_spec_digest.clone(),
                                    record.automatic_rollback_on_generation_failure,
                                ));
                            }
                            record.health = ControlHealth::Failed;
                            record.lifecycle = ControlLifecycle::Retired;
                            record.retirement_reason = Some(RetirementReason::StagingFailed);
                            if next.active_generation_spec_digest.as_deref()
                                == Some(&record.generation_spec_digest)
                            {
                                next.active_generation_spec_digest = None;
                            }
                            if matches!(error, ControlPlaneError::TransitionRejected { .. }) {
                                shutdown_recovered_slots(&mut runtime, slots, &state).await;
                                return Err(error);
                            }
                        }
                    }
                }
                ControlLifecycle::Staged | ControlLifecycle::Ready | ControlLifecycle::Draining => {
                    record.lifecycle = ControlLifecycle::Retired;
                    record.retirement_reason = Some(RetirementReason::RecoveryCleanup);
                }
                ControlLifecycle::Retired => {}
            }
        }
        if next.active_generation_spec_digest.is_none()
            && let Some((transition_digest, true)) = failed_active
            && let Some(standby) = next.generations.iter_mut().find(|record| {
                record.lifecycle == ControlLifecycle::Standby
                    && record.transition_spec_digest == transition_digest
                    && deadline(record).is_ok_and(|deadline| deadline > now_unix_nanos)
                    && slots.contains_key(&record.generation_spec_digest)
            })
        {
            standby.lifecycle = ControlLifecycle::Active;
            standby.activation_direction = ActivationDirection::Rollback;
            next.active_generation_spec_digest = Some(standby.generation_spec_digest.clone());
            next.routing_epoch =
                recover_or_cleanup!(next.routing_epoch.checked_add(1).ok_or_else(|| {
                    ControlPlaneError::TransitionRejected {
                        detail: "Routing Epoch exhausted".to_owned(),
                    }
                }));
        }
        next = recover_or_cleanup!(store.compare_and_swap(&app_id, state.revision, next));
        Ok(Self {
            app_id,
            runtime,
            store,
            state: next,
            slots,
        })
    }

    /// Stages, records Ready, and atomically switches one exact Generation edge.
    #[allow(clippy::too_many_lines)]
    pub async fn transition(
        &mut self,
        transition: &CanonicalDocument<AppGenerationTransitionSpec>,
        candidate: &ResolvedGeneration,
        receipts: &BTreeMap<String, CanonicalDocument<StateCompatibilityReceipt>>,
        now_unix_nanos: u128,
    ) -> Result<DurableTransitionOutcome, ControlPlaneError> {
        let edge = transition.value();
        validate_edge(
            &self.app_id,
            self.state.active_generation_spec_digest.as_deref(),
            edge,
            candidate,
        )?;
        let previous = self
            .state
            .active_generation_spec_digest
            .as_ref()
            .and_then(|digest| self.slots.get(digest))
            .map(|slot| &slot.generation);
        validate_state_compatibility(&self.app_id, previous, candidate, edge, receipts)?;
        let ready_timeout = parse_nanos(&edge.rollout_policy.ready_timeout_nanos, "ready timeout")?;
        let drain_timeout = parse_nanos(&edge.rollout_policy.drain_timeout_nanos, "drain timeout")?;
        let drain_deadline = now_unix_nanos
            .checked_add(u128::from(drain_timeout))
            .ok_or_else(|| ControlPlaneError::TransitionRejected {
                detail: "drain deadline exhausted".to_owned(),
            })?
            .to_string();
        let rollback_duration = parse_nanos(
            &edge.rollout_policy.rollback_window_nanos,
            "rollback window",
        )?;
        let rollback_deadline = if rollback_duration == 0 {
            None
        } else {
            Some(
                now_unix_nanos
                    .checked_add(u128::from(rollback_duration))
                    .ok_or_else(|| ControlPlaneError::TransitionRejected {
                        detail: "rollback deadline exhausted".to_owned(),
                    })?
                    .to_string(),
            )
        };
        let candidate_digest = candidate.spec.digest().to_owned();
        if self
            .record(&candidate_digest)
            .is_some_and(|record| record.lifecycle != ControlLifecycle::Retired)
        {
            return rejected("candidate Generation already has a control record");
        }

        let record = GenerationControlRecord {
            generation_spec_digest: candidate_digest.clone(),
            transition_spec_digest: transition.digest().to_owned(),
            lifecycle: ControlLifecycle::Staged,
            health: ControlHealth::Healthy,
            activation_direction: ActivationDirection::Forward,
            ready_timeout_nanos: edge.rollout_policy.ready_timeout_nanos.clone(),
            drain_timeout_nanos: edge.rollout_policy.drain_timeout_nanos.clone(),
            drain_deadline_unix_nanos: None,
            rollback_deadline_unix_nanos: rollback_deadline.clone(),
            automatic_rollback_on_generation_failure: edge
                .rollout_policy
                .automatic_rollback_on_generation_failure,
            state_compatibility_receipt_digests: edge.state_compatibility_receipt_digests.clone(),
            retirement_reason: None,
        };
        let mut next = self.state.clone();
        if let Some(retired) = next
            .generations
            .iter_mut()
            .find(|existing| existing.generation_spec_digest == candidate_digest)
        {
            *retired = record;
        } else {
            next.generations.push(record);
        }
        self.commit(next)?;

        if edge.replacement_mode == ReplacementMode::Maintenance {
            let active = self
                .state
                .active_generation_spec_digest
                .clone()
                .expect("maintenance edge requires active Generation");
            let slot = self.slots.get(&active).expect("active slot must be live");
            if slot.leases.get() != 0 {
                self.mark_record(&candidate_digest, |record| {
                    record.health = ControlHealth::Failed;
                    record.lifecycle = ControlLifecycle::Retired;
                    record.retirement_reason = Some(RetirementReason::StagingFailed);
                })?;
                return rejected("maintenance replacement cannot stop a leased Generation");
            }
            let slot = self
                .slots
                .remove(&active)
                .expect("active slot was validated");
            let shutdown = self.runtime.shutdown(slot.handle, drain_timeout).await;
            let mut next = self.state.clone();
            next.active_generation_spec_digest = None;
            update_record(&mut next, &active, |record| {
                record.lifecycle = ControlLifecycle::Retired;
                record.retirement_reason = Some(RetirementReason::Replaced);
                if shutdown.is_err() {
                    record.health = ControlHealth::Failed;
                }
            })?;
            if shutdown.is_err() {
                update_record(&mut next, &candidate_digest, |record| {
                    record.lifecycle = ControlLifecycle::Retired;
                    record.health = ControlHealth::Failed;
                    record.retirement_reason = Some(RetirementReason::StagingFailed);
                })?;
            }
            self.commit(next)?;
            shutdown?;
        }

        let handle = match self.runtime.stage(candidate, ready_timeout).await {
            Ok(handle) => handle,
            Err(error) => {
                self.mark_record(&candidate_digest, |record| {
                    record.health = ControlHealth::Failed;
                    record.lifecycle = ControlLifecycle::Retired;
                    record.retirement_reason = Some(RetirementReason::StagingFailed);
                })?;
                return Err(error);
            }
        };
        if let Err(error) = self.mark_record(&candidate_digest, |record| {
            record.lifecycle = ControlLifecycle::Ready;
        }) {
            let _ = self.runtime.shutdown(handle, drain_timeout).await;
            return Err(error);
        }

        let previous_digest = edge.from_generation_spec_digest.clone();
        let next = (|| {
            let mut next = self.state.clone();
            next.routing_epoch = next.routing_epoch.checked_add(1).ok_or_else(|| {
                ControlPlaneError::TransitionRejected {
                    detail: "Routing Epoch exhausted".to_owned(),
                }
            })?;
            next.active_generation_spec_digest = Some(candidate_digest.clone());
            update_record(&mut next, &candidate_digest, |record| {
                record.lifecycle = ControlLifecycle::Active;
            })?;
            if edge.replacement_mode == ReplacementMode::Overlap {
                let previous = previous_digest
                    .as_ref()
                    .expect("overlap edge requires predecessor");
                update_record(&mut next, previous, |record| {
                    record.lifecycle = ControlLifecycle::Draining;
                    record.drain_deadline_unix_nanos = Some(drain_deadline.clone());
                    transition
                        .digest()
                        .clone_into(&mut record.transition_spec_digest);
                    record
                        .rollback_deadline_unix_nanos
                        .clone_from(&rollback_deadline);
                    record.automatic_rollback_on_generation_failure =
                        edge.rollout_policy.automatic_rollback_on_generation_failure;
                    record
                        .state_compatibility_receipt_digests
                        .clone_from(&edge.state_compatibility_receipt_digests);
                })?;
            }
            Ok(next)
        })();
        let next = match next {
            Ok(next) => next,
            Err(error) => {
                let _ = self.runtime.shutdown(handle, drain_timeout).await;
                return Err(error);
            }
        };
        if let Err(error) = self.commit(next) {
            let _ = self.runtime.shutdown(handle, drain_timeout).await;
            return Err(error);
        }
        self.slots.insert(
            candidate_digest.clone(),
            LiveSlot {
                generation: candidate.clone(),
                handle,
                leases: Rc::new(Cell::new(0)),
            },
        );
        Ok(DurableTransitionOutcome {
            active_generation_spec_digest: candidate_digest,
            supervisor_epoch: self.state.supervisor_epoch,
            routing_epoch: self.state.routing_epoch,
            draining_generation_spec_digest: if edge.replacement_mode == ReplacementMode::Overlap {
                previous_digest
            } else {
                None
            },
            activation_direction: ActivationDirection::Forward,
        })
    }

    /// Acquires a route only if the caller observed the current durable Routing Epoch.
    pub fn lease_at_epoch(
        &self,
        routing_epoch: u64,
    ) -> Result<DurableGenerationLease, ControlPlaneError> {
        let durable = self.store.load(&self.app_id)?;
        if durable.supervisor_epoch != self.state.supervisor_epoch
            || durable.routing_epoch != self.state.routing_epoch
            || durable.revision != self.state.revision
            || durable.active_generation_spec_digest != self.state.active_generation_spec_digest
        {
            return rejected("Generation Supervisor was fenced by newer durable authority");
        }
        if routing_epoch != self.state.routing_epoch {
            return rejected("router presented an obsolete Routing Epoch");
        }
        let digest = self
            .state
            .active_generation_spec_digest
            .as_ref()
            .ok_or_else(|| ControlPlaneError::TransitionRejected {
                detail: "no active App Generation".to_owned(),
            })?;
        let slot = self
            .slots
            .get(digest)
            .ok_or_else(|| ControlPlaneError::TransitionRejected {
                detail: "active Generation has no recovered runtime handle".to_owned(),
            })?;
        slot.leases
            .set(slot.leases.get().checked_add(1).ok_or_else(|| {
                ControlPlaneError::TransitionRejected {
                    detail: "Generation lease count exhausted".to_owned(),
                }
            })?);
        Ok(DurableGenerationLease {
            generation_spec_digest: digest.clone(),
            supervisor_epoch: self.state.supervisor_epoch,
            routing_epoch,
            leases: slot.leases.clone(),
        })
    }

    pub fn lease(&self) -> Result<DurableGenerationLease, ControlPlaneError> {
        self.lease_at_epoch(self.state.routing_epoch)
    }

    /// Acquires one invocation target under the caller's observed Routing Epoch.
    pub fn route_at_epoch(
        &self,
        routing_epoch: u64,
    ) -> Result<DurableGenerationRoute<R::Route>, ControlPlaneError> {
        let lease = self.lease_at_epoch(routing_epoch)?;
        let slot = self
            .slots
            .get(lease.generation_spec_digest())
            .expect("leased Generation must have one live slot");
        Ok(DurableGenerationRoute {
            target: self.runtime.route(&slot.handle),
            lease,
        })
    }

    /// Acquires the current invocation target and pins it until the route is dropped.
    pub fn route(&self) -> Result<DurableGenerationRoute<R::Route>, ControlPlaneError> {
        self.route_at_epoch(self.state.routing_epoch)
    }

    /// Moves a fully drained old Generation to standby or shuts it down after expiry.
    pub async fn complete_drain(
        &mut self,
        digest: &str,
        now_unix_nanos: u128,
    ) -> Result<(), ControlPlaneError> {
        let slot = self
            .slots
            .get(digest)
            .ok_or_else(|| unknown_generation(digest))?;
        if slot.leases.get() != 0
            || self.record(digest).map(|record| record.lifecycle)
                != Some(ControlLifecycle::Draining)
        {
            return rejected("Generation is not draining or still has active leases");
        }
        if self
            .record(digest)
            .is_some_and(|record| record.health == ControlHealth::Failed)
        {
            return self
                .shutdown_and_retire(digest, RetirementReason::TerminalFailure)
                .await;
        }
        if deadline_option(
            self.record(digest)
                .ok_or_else(|| unknown_generation(digest))?,
        )?
        .is_some_and(|deadline| deadline > now_unix_nanos)
        {
            return self.mark_record(digest, |record| {
                record.lifecycle = ControlLifecycle::Standby;
                record.drain_deadline_unix_nanos = None;
            });
        }
        self.shutdown_and_retire(digest, RetirementReason::Drained)
            .await
    }

    /// Performs the restricted reverse edge while the exact standby window is live.
    pub fn rollback(
        &mut self,
        standby_digest: &str,
        now_unix_nanos: u128,
    ) -> Result<DurableTransitionOutcome, ControlPlaneError> {
        self.rollback_inner(standby_digest, now_unix_nanos, false)
    }

    /// Marks terminal Generation failure and performs policy-authorized automatic rollback.
    pub fn mark_generation_failed(
        &mut self,
        digest: &str,
        now_unix_nanos: u128,
    ) -> Result<Option<DurableTransitionOutcome>, ControlPlaneError> {
        let record = self
            .record(digest)
            .ok_or_else(|| unknown_generation(digest))?;
        let automatic = record.automatic_rollback_on_generation_failure;
        let transition_digest = record.transition_spec_digest.clone();
        self.mark_record(digest, |record| record.health = ControlHealth::Failed)?;
        if self.state.active_generation_spec_digest.as_deref() != Some(digest) {
            return Ok(None);
        }
        let standby = automatic.then(|| {
            self.state
                .generations
                .iter()
                .find(|record| {
                    matches!(
                        record.lifecycle,
                        ControlLifecycle::Draining | ControlLifecycle::Standby
                    ) && record.transition_spec_digest == transition_digest
                        && record.health == ControlHealth::Healthy
                        && deadline(record).is_ok_and(|deadline| deadline > now_unix_nanos)
                })
                .map(|record| record.generation_spec_digest.clone())
        });
        if let Some(Some(standby)) = standby {
            return self
                .rollback_inner(&standby, now_unix_nanos, true)
                .map(Some);
        }
        self.fence_failed_active(digest, now_unix_nanos)?;
        Ok(None)
    }

    /// Reconciles the active Generation's runtime health with durable control authority.
    ///
    /// A terminal runtime failure is recorded exactly once. When the active
    /// transition authorized automatic rollback and retains an exact standby,
    /// this call atomically switches the route back to that Generation.
    pub fn reconcile_active_generation(
        &mut self,
        now_unix_nanos: u128,
    ) -> Result<Option<GenerationFailureOutcome>, ControlPlaneError> {
        let Some(digest) = self.state.active_generation_spec_digest.clone() else {
            return Ok(None);
        };
        let record = self.record(&digest).expect("active record must exist");
        if record.health == ControlHealth::Failed {
            return Ok(None);
        }
        let slot = self.slots.get(&digest).expect("active slot must be live");
        let Some(failure) = self.runtime.terminal_failure(&slot.handle) else {
            return Ok(None);
        };
        let automatic_rollback = self.mark_generation_failed(&digest, now_unix_nanos)?;
        Ok(Some(GenerationFailureOutcome {
            generation_spec_digest: digest,
            failure,
            automatic_rollback,
        }))
    }

    /// Advances every automatic Generation lifecycle edge due at `now_unix_nanos`.
    pub async fn maintain(
        &mut self,
        now_unix_nanos: u128,
    ) -> Result<Vec<GenerationMaintenanceOutcome>, ControlPlaneError> {
        let mut outcomes = Vec::new();
        if let Some(failure) = self.reconcile_active_generation(now_unix_nanos)? {
            outcomes.push(GenerationMaintenanceOutcome::Failed(failure));
        }
        outcomes.extend(self.reconcile_inactive_failures(now_unix_nanos).await?);
        let candidates = self
            .state
            .generations
            .iter()
            .filter(|record| {
                matches!(
                    record.lifecycle,
                    ControlLifecycle::Draining | ControlLifecycle::Standby
                )
            })
            .map(|record| record.generation_spec_digest.clone())
            .collect::<Vec<_>>();
        for digest in candidates {
            if let Some(outcome) = self.maintain_candidate(&digest, now_unix_nanos).await? {
                outcomes.push(outcome);
            }
        }
        Ok(outcomes)
    }

    async fn reconcile_inactive_failures(
        &mut self,
        now_unix_nanos: u128,
    ) -> Result<Vec<GenerationMaintenanceOutcome>, ControlPlaneError> {
        let active = self.state.active_generation_spec_digest.clone();
        let inactive_failures = self
            .state
            .generations
            .iter()
            .filter(|record| {
                record.health == ControlHealth::Healthy
                    && matches!(
                        record.lifecycle,
                        ControlLifecycle::Draining | ControlLifecycle::Standby
                    )
                    && active.as_deref() != Some(&record.generation_spec_digest)
            })
            .filter_map(|record| {
                let slot = self.slots.get(&record.generation_spec_digest)?;
                self.runtime
                    .terminal_failure(&slot.handle)
                    .map(|failure| (record.generation_spec_digest.clone(), failure))
            })
            .collect::<Vec<_>>();
        let mut outcomes = Vec::new();
        for (digest, failure) in inactive_failures {
            self.mark_generation_failed(&digest, now_unix_nanos)?;
            self.shutdown_and_retire(&digest, RetirementReason::TerminalFailure)
                .await?;
            outcomes.push(GenerationMaintenanceOutcome::Failed(
                GenerationFailureOutcome {
                    generation_spec_digest: digest.clone(),
                    failure,
                    automatic_rollback: None,
                },
            ));
            outcomes.push(GenerationMaintenanceOutcome::Retired {
                generation_spec_digest: digest,
                reason: RetirementReason::TerminalFailure,
            });
        }
        Ok(outcomes)
    }

    async fn maintain_candidate(
        &mut self,
        digest: &str,
        now_unix_nanos: u128,
    ) -> Result<Option<GenerationMaintenanceOutcome>, ControlPlaneError> {
        let record = self
            .record(digest)
            .ok_or_else(|| unknown_generation(digest))?;
        if record.lifecycle == ControlLifecycle::Standby {
            if deadline(record)? > now_unix_nanos {
                return Ok(None);
            }
            self.shutdown_and_retire(digest, RetirementReason::RollbackWindowExpired)
                .await?;
            return Ok(Some(GenerationMaintenanceOutcome::Retired {
                generation_spec_digest: digest.to_owned(),
                reason: RetirementReason::RollbackWindowExpired,
            }));
        }
        if record.lifecycle != ControlLifecycle::Draining {
            return Ok(None);
        }
        if record.health == ControlHealth::Failed {
            self.shutdown_and_retire(digest, RetirementReason::TerminalFailure)
                .await?;
            return Ok(Some(GenerationMaintenanceOutcome::Retired {
                generation_spec_digest: digest.to_owned(),
                reason: RetirementReason::TerminalFailure,
            }));
        }
        let leases = self
            .slots
            .get(digest)
            .ok_or_else(|| unknown_generation(digest))?
            .leases
            .get();
        if leases == 0 && deadline_option(record)?.is_some_and(|deadline| deadline > now_unix_nanos)
        {
            self.mark_record(digest, |record| {
                record.lifecycle = ControlLifecycle::Standby;
                record.drain_deadline_unix_nanos = None;
            })?;
            return Ok(Some(GenerationMaintenanceOutcome::Standby {
                generation_spec_digest: digest.to_owned(),
            }));
        }
        let reason = if leases == 0 {
            record
                .retirement_reason
                .unwrap_or(RetirementReason::Drained)
        } else if drain_deadline(record)? <= now_unix_nanos {
            RetirementReason::DrainDeadlineExceeded
        } else {
            return Ok(None);
        };
        self.shutdown_and_retire(digest, reason).await?;
        Ok(Some(GenerationMaintenanceOutcome::Retired {
            generation_spec_digest: digest.to_owned(),
            reason,
        }))
    }

    /// Stops this Host incarnation while preserving durable Generation routing authority.
    ///
    /// The Supervisor must terminate immediately after success. A later Host uses `recover` to
    /// restage the exact durable Active and Standby Generations.
    pub async fn suspend_host(&mut self) -> Result<DurableControlState, ControlPlaneError> {
        if self.slots.values().any(|slot| slot.leases.get() != 0) {
            return rejected("cannot suspend Host while a Generation lease is active");
        }
        let live = self
            .slots
            .keys()
            .map(|digest| {
                let record = self
                    .record(digest)
                    .expect("every live slot has one durable record");
                parse_nanos(&record.drain_timeout_nanos, "drain timeout")
                    .map(|timeout| (digest.clone(), timeout))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut failures = Vec::new();
        for (digest, timeout) in live {
            let slot = self
                .slots
                .remove(&digest)
                .expect("the live slot list was closed before suspension");
            if let Err(error) = self.runtime.shutdown(slot.handle, timeout).await {
                failures.push(format!("Generation `{digest}`: {error:?}"));
            }
        }
        if failures.is_empty() {
            let mut next = self.state.clone();
            next.host_suspended = true;
            self.commit(next)?;
            Ok(self.state.clone())
        } else {
            Err(ControlPlaneError::HostFailure {
                detail: format!("Host suspension cleanup failed: {}", failures.join("; ")),
            })
        }
    }

    /// Fences new routes and begins bounded retirement of every live Generation.
    pub fn begin_shutdown(&mut self, now_unix_nanos: u128) -> Result<(), ControlPlaneError> {
        let mut next = self.state.clone();
        if next.active_generation_spec_digest.take().is_some() {
            next.routing_epoch = next.routing_epoch.checked_add(1).ok_or_else(|| {
                ControlPlaneError::TransitionRejected {
                    detail: "Routing Epoch exhausted".to_owned(),
                }
            })?;
        }
        for record in &mut next.generations {
            if matches!(
                record.lifecycle,
                ControlLifecycle::Active | ControlLifecycle::Draining | ControlLifecycle::Standby
            ) {
                let timeout = parse_nanos(&record.drain_timeout_nanos, "drain timeout")?;
                record.lifecycle = ControlLifecycle::Draining;
                record.retirement_reason = Some(RetirementReason::SupervisorShutdown);
                record.rollback_deadline_unix_nanos = None;
                record.drain_deadline_unix_nanos = Some(
                    now_unix_nanos
                        .checked_add(u128::from(timeout))
                        .ok_or_else(|| ControlPlaneError::TransitionRejected {
                            detail: "drain deadline exhausted".to_owned(),
                        })?
                        .to_string(),
                );
            }
        }
        self.commit(next)
    }

    pub fn is_retired(&self) -> bool {
        self.slots.is_empty()
    }

    /// Expires one standby window and releases its Generation-owned resources.
    pub async fn expire_rollback_window(
        &mut self,
        digest: &str,
        now_unix_nanos: u128,
    ) -> Result<(), ControlPlaneError> {
        let record = self
            .record(digest)
            .ok_or_else(|| unknown_generation(digest))?;
        if record.lifecycle != ControlLifecycle::Standby || deadline(record)? > now_unix_nanos {
            return rejected("rollback standby window has not expired");
        }
        self.shutdown_and_retire(digest, RetirementReason::RollbackWindowExpired)
            .await
    }

    pub const fn state(&self) -> &DurableControlState {
        &self.state
    }

    fn rollback_inner(
        &mut self,
        standby_digest: &str,
        now_unix_nanos: u128,
        automatic: bool,
    ) -> Result<DurableTransitionOutcome, ControlPlaneError> {
        let standby = self
            .record(standby_digest)
            .ok_or_else(|| unknown_generation(standby_digest))?;
        if !matches!(
            standby.lifecycle,
            ControlLifecycle::Draining | ControlLifecycle::Standby
        ) || deadline(standby)? <= now_unix_nanos
        {
            return rejected("rollback target is not a retained predecessor");
        }
        let failed = self
            .state
            .active_generation_spec_digest
            .clone()
            .ok_or_else(|| ControlPlaneError::TransitionRejected {
                detail: "rollback requires one active Generation".to_owned(),
            })?;
        let active = self.record(&failed).expect("active record must exist");
        if active.transition_spec_digest != standby.transition_spec_digest {
            return rejected("rollback records do not bind the same Transition Spec");
        }
        if automatic && active.health != ControlHealth::Failed {
            return rejected("automatic rollback requires terminal active Generation failure");
        }
        let failed_drain_timeout = parse_nanos(&active.drain_timeout_nanos, "drain timeout")?;
        let failed_drain_deadline = now_unix_nanos
            .checked_add(u128::from(failed_drain_timeout))
            .ok_or_else(|| ControlPlaneError::TransitionRejected {
                detail: "drain deadline exhausted".to_owned(),
            })?
            .to_string();
        let mut next = self.state.clone();
        next.routing_epoch = next.routing_epoch.checked_add(1).ok_or_else(|| {
            ControlPlaneError::TransitionRejected {
                detail: "Routing Epoch exhausted".to_owned(),
            }
        })?;
        next.active_generation_spec_digest = Some(standby_digest.to_owned());
        update_record(&mut next, standby_digest, |record| {
            record.lifecycle = ControlLifecycle::Active;
            record.activation_direction = ActivationDirection::Rollback;
            record.drain_deadline_unix_nanos = None;
        })?;
        update_record(&mut next, &failed, |record| {
            record.lifecycle = ControlLifecycle::Draining;
            record.drain_deadline_unix_nanos = Some(failed_drain_deadline);
        })?;
        self.commit(next)?;
        Ok(DurableTransitionOutcome {
            active_generation_spec_digest: standby_digest.to_owned(),
            supervisor_epoch: self.state.supervisor_epoch,
            routing_epoch: self.state.routing_epoch,
            draining_generation_spec_digest: Some(failed),
            activation_direction: ActivationDirection::Rollback,
        })
    }

    fn fence_failed_active(
        &mut self,
        digest: &str,
        now_unix_nanos: u128,
    ) -> Result<(), ControlPlaneError> {
        let record = self
            .record(digest)
            .ok_or_else(|| unknown_generation(digest))?;
        let timeout = parse_nanos(&record.drain_timeout_nanos, "drain timeout")?;
        let drain_deadline = now_unix_nanos
            .checked_add(u128::from(timeout))
            .ok_or_else(|| ControlPlaneError::TransitionRejected {
                detail: "drain deadline exhausted".to_owned(),
            })?
            .to_string();
        let mut next = self.state.clone();
        if next.active_generation_spec_digest.as_deref() != Some(digest) {
            return rejected("terminal failure fence requires the active Generation");
        }
        next.active_generation_spec_digest = None;
        next.routing_epoch = next.routing_epoch.checked_add(1).ok_or_else(|| {
            ControlPlaneError::TransitionRejected {
                detail: "Routing Epoch exhausted".to_owned(),
            }
        })?;
        update_record(&mut next, digest, |record| {
            record.lifecycle = ControlLifecycle::Draining;
            record.drain_deadline_unix_nanos = Some(drain_deadline);
        })?;
        self.commit(next)
    }

    async fn shutdown_and_retire(
        &mut self,
        digest: &str,
        reason: RetirementReason,
    ) -> Result<(), ControlPlaneError> {
        let timeout = parse_nanos(
            &self
                .record(digest)
                .ok_or_else(|| unknown_generation(digest))?
                .drain_timeout_nanos,
            "drain timeout",
        )?;
        let slot = self
            .slots
            .remove(digest)
            .ok_or_else(|| unknown_generation(digest))?;
        let shutdown = self.runtime.shutdown(slot.handle, timeout).await;
        self.mark_record(digest, |record| {
            record.lifecycle = ControlLifecycle::Retired;
            record.drain_deadline_unix_nanos = None;
            record.rollback_deadline_unix_nanos = None;
            record.retirement_reason = Some(reason);
            if shutdown.is_err() {
                record.health = ControlHealth::Failed;
            }
        })?;
        shutdown
    }

    fn record(&self, digest: &str) -> Option<&GenerationControlRecord> {
        self.state
            .generations
            .iter()
            .find(|record| record.generation_spec_digest == digest)
    }

    fn mark_record(
        &mut self,
        digest: &str,
        update: impl FnOnce(&mut GenerationControlRecord),
    ) -> Result<(), ControlPlaneError> {
        let mut next = self.state.clone();
        update_record(&mut next, digest, update)?;
        self.commit(next)
    }

    fn commit(&mut self, next: DurableControlState) -> Result<(), ControlPlaneError> {
        self.state = self
            .store
            .compare_and_swap(&self.app_id, self.state.revision, next)?;
        Ok(())
    }
}

async fn shutdown_recovered_slots<R: GenerationRuntime>(
    runtime: &mut R,
    slots: BTreeMap<String, LiveSlot<R::Handle>>,
    state: &DurableControlState,
) {
    for (digest, slot) in slots {
        let timeout = state
            .generations
            .iter()
            .find(|record| record.generation_spec_digest == digest)
            .and_then(|record| record.drain_timeout_nanos.parse::<u64>().ok())
            .unwrap_or(0);
        let _ = runtime.shutdown(slot.handle, timeout).await;
    }
}

fn update_record(
    state: &mut DurableControlState,
    digest: &str,
    update: impl FnOnce(&mut GenerationControlRecord),
) -> Result<(), ControlPlaneError> {
    let record = state
        .generations
        .iter_mut()
        .find(|record| record.generation_spec_digest == digest)
        .ok_or_else(|| unknown_generation(digest))?;
    update(record);
    Ok(())
}

fn deadline(record: &GenerationControlRecord) -> Result<u128, ControlPlaneError> {
    deadline_option(record)?.ok_or_else(|| ControlPlaneError::TransitionRejected {
        detail: "Generation has no rollback deadline".to_owned(),
    })
}

fn drain_deadline(record: &GenerationControlRecord) -> Result<u128, ControlPlaneError> {
    record
        .drain_deadline_unix_nanos
        .as_deref()
        .ok_or_else(|| ControlPlaneError::TransitionRejected {
            detail: "draining Generation has no drain deadline".to_owned(),
        })?
        .parse::<u128>()
        .map_err(|_| ControlPlaneError::TransitionRejected {
            detail: "drain deadline is not a bounded unsigned integer".to_owned(),
        })
}

fn deadline_option(record: &GenerationControlRecord) -> Result<Option<u128>, ControlPlaneError> {
    record
        .rollback_deadline_unix_nanos
        .as_deref()
        .map(|value| {
            value
                .parse::<u128>()
                .map_err(|_| ControlPlaneError::TransitionRejected {
                    detail: "rollback deadline is not a bounded unsigned integer".to_owned(),
                })
        })
        .transpose()
}

fn parse_nanos(value: &str, kind: &str) -> Result<u64, ControlPlaneError> {
    value
        .parse::<u64>()
        .map_err(|_| ControlPlaneError::TransitionRejected {
            detail: format!("{kind} is not a bounded unsigned integer"),
        })
}

fn unknown_generation(digest: &str) -> ControlPlaneError {
    ControlPlaneError::TransitionRejected {
        detail: format!("unknown Generation `{digest}`"),
    }
}

fn rejected<T>(detail: impl Into<String>) -> Result<T, ControlPlaneError> {
    Err(ControlPlaneError::TransitionRejected {
        detail: detail.into(),
    })
}

#[cfg(test)]
#[path = "durable_supervisor_tests.rs"]
mod tests;
