use std::{
    cell::RefCell,
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::{CanonicalDocument, ControlPlaneError};

/// Durable lifecycle of one complete App Generation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlLifecycle {
    Staged,
    Ready,
    Active,
    Draining,
    Standby,
    Retired,
}

/// Durable health is orthogonal to lifecycle.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlHealth {
    Healthy,
    Failed,
}

/// Direction which most recently activated a Generation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationDirection {
    Forward,
    Rollback,
}

/// Durable reason why a Generation released all Host-owned resources.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetirementReason {
    StagingFailed,
    Replaced,
    Drained,
    DrainDeadlineExceeded,
    RollbackWindowExpired,
    TerminalFailure,
    SupervisorShutdown,
    RecoveryCleanup,
}

/// Durable authority and recovery facts for one Generation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationControlRecord {
    pub generation_spec_digest: String,
    pub transition_spec_digest: String,
    pub lifecycle: ControlLifecycle,
    pub health: ControlHealth,
    pub activation_direction: ActivationDirection,
    pub ready_timeout_nanos: String,
    pub drain_timeout_nanos: String,
    #[serde(default)]
    pub drain_deadline_unix_nanos: Option<String>,
    pub rollback_deadline_unix_nanos: Option<String>,
    pub automatic_rollback_on_generation_failure: bool,
    pub state_compatibility_receipt_digests: Vec<String>,
    #[serde(default)]
    pub retirement_reason: Option<RetirementReason>,
}

/// Compare-and-swap state read before route admission or recovery.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DurableControlState {
    pub schema_version: u32,
    pub app_id: String,
    pub revision: u64,
    pub supervisor_epoch: u64,
    pub routing_epoch: u64,
    pub active_generation_spec_digest: Option<String>,
    pub generations: Vec<GenerationControlRecord>,
}

impl DurableControlState {
    pub(crate) fn initial(app_id: &str) -> Self {
        Self {
            schema_version: 1,
            app_id: app_id.to_owned(),
            revision: 0,
            supervisor_epoch: 0,
            routing_epoch: 0,
            active_generation_spec_digest: None,
            generations: Vec::new(),
        }
    }

    pub(crate) fn validate(&self, app_id: &str) -> Result<(), ControlPlaneError> {
        let unique = self
            .generations
            .iter()
            .map(|record| &record.generation_spec_digest)
            .collect::<BTreeSet<_>>();
        if self.schema_version != 1
            || self.app_id != app_id
            || unique.len() != self.generations.len()
            || self
                .active_generation_spec_digest
                .as_ref()
                .is_some_and(|active| {
                    !self.generations.iter().any(|record| {
                        &record.generation_spec_digest == active
                            && record.lifecycle == ControlLifecycle::Active
                    })
                })
            || self
                .generations
                .iter()
                .filter(|record| record.lifecycle == ControlLifecycle::Active)
                .count()
                != usize::from(self.active_generation_spec_digest.is_some())
        {
            return Err(ControlPlaneError::TransitionRejected {
                detail: "durable control state violates App, uniqueness, or active-route closure"
                    .to_owned(),
            });
        }
        Ok(())
    }
}

/// Linearizable persistence seam for fenced supervisor state.
pub trait ControlStateStore: std::fmt::Debug {
    fn load(&self, app_id: &str) -> Result<DurableControlState, ControlPlaneError>;
    fn compare_and_swap(
        &self,
        app_id: &str,
        expected_revision: u64,
        next: DurableControlState,
    ) -> Result<DurableControlState, ControlPlaneError>;
}

/// In-memory CAS backend for embedding and deterministic tests.
#[derive(Debug, Default)]
pub struct MemoryControlStateStore {
    state: RefCell<Option<DurableControlState>>,
}

impl ControlStateStore for MemoryControlStateStore {
    fn load(&self, app_id: &str) -> Result<DurableControlState, ControlPlaneError> {
        let state = self
            .state
            .borrow()
            .clone()
            .unwrap_or_else(|| DurableControlState::initial(app_id));
        state.validate(app_id)?;
        Ok(state)
    }

    fn compare_and_swap(
        &self,
        app_id: &str,
        expected_revision: u64,
        mut next: DurableControlState,
    ) -> Result<DurableControlState, ControlPlaneError> {
        let current = self.load(app_id)?;
        if current.revision != expected_revision {
            return fenced(current.revision, expected_revision);
        }
        next.revision = expected_revision.checked_add(1).ok_or_else(|| {
            ControlPlaneError::TransitionRejected {
                detail: "control-state revision exhausted".to_owned(),
            }
        })?;
        next.validate(app_id)?;
        *self.state.borrow_mut() = Some(next.clone());
        Ok(next)
    }
}

/// File-backed, fsync'd CAS state protected by an OS advisory lock.
#[derive(Clone, Debug)]
pub struct FileControlStateStore {
    root: PathBuf,
}

impl FileControlStateStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, ControlPlaneError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(store_error)?;
        Ok(Self { root })
    }

    fn state_path(&self) -> PathBuf {
        self.root.join("control-state.json")
    }

    fn lock(&self) -> Result<File, ControlPlaneError> {
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join("control-state.lock"))
            .map_err(store_error)?;
        FileExt::lock_exclusive(&lock).map_err(store_error)?;
        Ok(lock)
    }

    fn load_locked(&self, app_id: &str) -> Result<DurableControlState, ControlPlaneError> {
        let path = self.state_path();
        let state = match fs::read(&path) {
            Ok(bytes) => {
                CanonicalDocument::<DurableControlState>::parse("control-state.json", &bytes)?
                    .into_value()
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                DurableControlState::initial(app_id)
            }
            Err(error) => return Err(store_error(error)),
        };
        state.validate(app_id)?;
        Ok(state)
    }

    fn write_locked(&self, state: &DurableControlState) -> Result<(), ControlPlaneError> {
        let document = CanonicalDocument::from_value("control-state.json", state.clone())?;
        let temporary = temporary_path(&self.root, state.revision)?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(store_error)?;
        let result = (|| {
            file.write_all(document.bytes()).map_err(store_error)?;
            file.sync_all().map_err(store_error)?;
            fs::rename(&temporary, self.state_path()).map_err(store_error)?;
            File::open(&self.root)
                .and_then(|directory| directory.sync_all())
                .map_err(store_error)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

impl ControlStateStore for FileControlStateStore {
    fn load(&self, app_id: &str) -> Result<DurableControlState, ControlPlaneError> {
        let lock = self.lock()?;
        let result = self.load_locked(app_id);
        let _ = FileExt::unlock(&lock);
        result
    }

    fn compare_and_swap(
        &self,
        app_id: &str,
        expected_revision: u64,
        mut next: DurableControlState,
    ) -> Result<DurableControlState, ControlPlaneError> {
        let lock = self.lock()?;
        let result = (|| {
            let current = self.load_locked(app_id)?;
            if current.revision != expected_revision {
                return fenced(current.revision, expected_revision);
            }
            next.revision = expected_revision.checked_add(1).ok_or_else(|| {
                ControlPlaneError::TransitionRejected {
                    detail: "control-state revision exhausted".to_owned(),
                }
            })?;
            next.validate(app_id)?;
            self.write_locked(&next)?;
            Ok(next)
        })();
        let _ = FileExt::unlock(&lock);
        result
    }
}

fn temporary_path(root: &Path, revision: u64) -> Result<PathBuf, ControlPlaneError> {
    for attempt in 0_u16..=u16::MAX {
        let path = root.join(format!(
            ".control-state-{}-{revision}-{attempt}.tmp",
            std::process::id()
        ));
        if !path.exists() {
            return Ok(path);
        }
    }
    Err(ControlPlaneError::StoreFailure {
        detail: "cannot allocate a control-state temporary file".to_owned(),
    })
}

fn fenced<T>(current: u64, expected: u64) -> Result<T, ControlPlaneError> {
    Err(ControlPlaneError::TransitionRejected {
        detail: format!(
            "control-state compare-and-swap fenced: expected revision {expected}, current {current}"
        ),
    })
}

fn store_error(error: impl std::fmt::Display) -> ControlPlaneError {
    ControlPlaneError::StoreFailure {
        detail: format!("control-state persistence failed: {error}"),
    }
}
