use std::fmt;

use lenso_kernel::{RuntimeFailure, ShutdownOutcome};

/// A native Runner startup or lane-lifecycle failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplicatedRunnerError {
    /// The immutable Plan failed validation before any lane started.
    InvalidPlan { detail: String },
    /// One lane could not construct or start its Kernel replica.
    LaneStartup { lane: String, detail: String },
    /// A generated request Capability was not registered with the native transfer catalog.
    MissingCrossLaneRequestTransfer { capability: String },
    /// A generated stream Capability was not registered with the native transfer catalog.
    MissingCrossLaneStreamTransfer { capability: String },
    /// A generated Event Capability was not registered with the native transfer catalog.
    MissingCrossLaneEventTransfer { capability: String },
    /// A lane stopped accepting Runner commands unexpectedly.
    LaneUnavailable { lane: String },
    /// A lane thread panicked while starting, running, or stopping.
    LanePanicked { lane: String },
    /// One Kernel replica reached a terminal runtime failure.
    LaneRuntimeFailure { lane: String, error: RuntimeFailure },
    /// One Kernel replica did not stop cleanly.
    LaneShutdown {
        lane: String,
        outcome: ShutdownOutcome,
    },
}

impl fmt::Display for ReplicatedRunnerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPlan { detail } => {
                write!(formatter, "invalid Resolved App Plan: {detail}")
            }
            Self::LaneStartup { lane, detail } => {
                write!(
                    formatter,
                    "Execution Lane `{lane}` failed to start: {detail}"
                )
            }
            Self::MissingCrossLaneRequestTransfer { capability } => write!(
                formatter,
                "Capability `{capability}` has no registered native cross-lane request transfer"
            ),
            Self::MissingCrossLaneStreamTransfer { capability } => write!(
                formatter,
                "Capability `{capability}` has no registered native cross-lane stream transfer"
            ),
            Self::MissingCrossLaneEventTransfer { capability } => write!(
                formatter,
                "Capability `{capability}` has no registered native cross-lane Event transfer"
            ),
            Self::LaneUnavailable { lane } => {
                write!(formatter, "Execution Lane `{lane}` is unavailable")
            }
            Self::LanePanicked { lane } => write!(formatter, "Execution Lane `{lane}` panicked"),
            Self::LaneRuntimeFailure { lane, error } => write!(
                formatter,
                "Execution Lane `{lane}` reached a terminal runtime failure: {error:?}"
            ),
            Self::LaneShutdown { lane, outcome } => write!(
                formatter,
                "Execution Lane `{lane}` stopped with {outcome:?}"
            ),
        }
    }
}

impl std::error::Error for ReplicatedRunnerError {}
