use std::{cell::Cell, collections::BTreeMap, rc::Rc};

use crate::{
    ActivationDirection, AppGenerationTransitionSpec, CanonicalDocument, ControlHealth,
    ControlLifecycle, ControlPlaneError, ControlStateStore, DurableControlState,
    GenerationControlRecord, GenerationRuntime, ReplacementMode, ResolvedGeneration,
    StateCompatibilityReceipt,
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
        let mut failed_active: Option<(String, bool)> = None;
        let mut next = state.clone();
        for record in &mut next.generations {
            match record.lifecycle {
                ControlLifecycle::Active | ControlLifecycle::Standby => {
                    let generation =
                        generations
                            .get(&record.generation_spec_digest)
                            .ok_or_else(|| ControlPlaneError::TransitionRejected {
                                detail: format!(
                                    "recovery lacks exact Generation `{}` authority",
                                    record.generation_spec_digest
                                ),
                            })?;
                    if generation.spec.digest() != record.generation_spec_digest
                        || generation.spec.value().app_id != app_id
                    {
                        return rejected("recovery Generation authority digest or App mismatch");
                    }
                    if record.lifecycle == ControlLifecycle::Standby
                        && deadline(record)? <= now_unix_nanos
                    {
                        record.lifecycle = ControlLifecycle::Retired;
                        continue;
                    }
                    let timeout = parse_nanos(&record.ready_timeout_nanos, "ready timeout")?;
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
                            if next.active_generation_spec_digest.as_deref()
                                == Some(&record.generation_spec_digest)
                            {
                                next.active_generation_spec_digest = None;
                            }
                            if matches!(error, ControlPlaneError::TransitionRejected { .. }) {
                                return Err(error);
                            }
                        }
                    }
                }
                ControlLifecycle::Staged | ControlLifecycle::Ready | ControlLifecycle::Draining => {
                    record.lifecycle = ControlLifecycle::Retired;
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
            next.routing_epoch = next.routing_epoch.checked_add(1).ok_or_else(|| {
                ControlPlaneError::TransitionRejected {
                    detail: "Routing Epoch exhausted".to_owned(),
                }
            })?;
        }
        next = store.compare_and_swap(&app_id, state.revision, next)?;
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
        if self.record(&candidate_digest).is_some() {
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
            rollback_deadline_unix_nanos: rollback_deadline.clone(),
            automatic_rollback_on_generation_failure: edge
                .rollout_policy
                .automatic_rollback_on_generation_failure,
            state_compatibility_receipt_digests: edge.state_compatibility_receipt_digests.clone(),
        };
        let mut next = self.state.clone();
        next.generations.push(record);
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
                })?;
                return rejected("maintenance replacement cannot stop a leased Generation");
            }
            let slot = self
                .slots
                .remove(&active)
                .expect("active slot was validated");
            self.runtime.shutdown(slot.handle, drain_timeout).await?;
            let mut next = self.state.clone();
            next.active_generation_spec_digest = None;
            update_record(&mut next, &active, |record| {
                record.lifecycle = ControlLifecycle::Retired;
            })?;
            self.commit(next)?;
        }

        let handle = match self.runtime.stage(candidate, ready_timeout).await {
            Ok(handle) => handle,
            Err(error) => {
                self.mark_record(&candidate_digest, |record| {
                    record.health = ControlHealth::Failed;
                    record.lifecycle = ControlLifecycle::Retired;
                })?;
                return Err(error);
            }
        };
        self.mark_record(&candidate_digest, |record| {
            record.lifecycle = ControlLifecycle::Ready;
        })?;

        let previous_digest = edge.from_generation_spec_digest.clone();
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
            return self.shutdown_and_retire(digest).await;
        }
        if deadline_option(
            self.record(digest)
                .ok_or_else(|| unknown_generation(digest))?,
        )?
        .is_some_and(|deadline| deadline > now_unix_nanos)
        {
            return self.mark_record(digest, |record| {
                record.lifecycle = ControlLifecycle::Standby;
            });
        }
        self.shutdown_and_retire(digest).await
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
        if self.state.active_generation_spec_digest.as_deref() != Some(digest) || !automatic {
            return Ok(None);
        }
        let standby = self
            .state
            .generations
            .iter()
            .find(|record| {
                record.lifecycle == ControlLifecycle::Standby
                    && record.transition_spec_digest == transition_digest
            })
            .map(|record| record.generation_spec_digest.clone())
            .ok_or_else(|| ControlPlaneError::TransitionRejected {
                detail: "automatic rollback has no exact retained standby Generation".to_owned(),
            })?;
        self.rollback_inner(&standby, now_unix_nanos, true)
            .map(Some)
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
        self.shutdown_and_retire(digest).await
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
        if standby.lifecycle != ControlLifecycle::Standby || deadline(standby)? <= now_unix_nanos {
            return rejected("rollback target is not retained in an unexpired standby window");
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
        })?;
        update_record(&mut next, &failed, |record| {
            record.lifecycle = ControlLifecycle::Draining;
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

    async fn shutdown_and_retire(&mut self, digest: &str) -> Result<(), ControlPlaneError> {
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
        self.runtime.shutdown(slot.handle, timeout).await?;
        self.mark_record(digest, |record| {
            record.lifecycle = ControlLifecycle::Retired;
        })
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
