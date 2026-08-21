use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use lenso_app_plan::{ExecutionLaneId, ResolvedAppPlan};
use lenso_kernel::NativeApp;

#[derive(Debug)]
pub(super) struct LaneDiagnosticsState {
    plan: Arc<ResolvedAppPlan>,
    lane_cpu_nanos: BTreeMap<String, AtomicU64>,
    instance_queue_depths: BTreeMap<String, AtomicU64>,
    total_messages: AtomicU64,
    cross_lane_messages: AtomicU64,
}

impl LaneDiagnosticsState {
    pub(super) fn new(plan: Arc<ResolvedAppPlan>) -> Self {
        let lane_cpu_nanos = plan
            .execution_lanes()
            .iter()
            .map(|lane| (lane.id().to_string(), AtomicU64::new(0)))
            .collect();
        let instance_queue_depths = plan
            .module_instances()
            .iter()
            .map(|instance| (instance.instance_key().to_owned(), AtomicU64::new(0)))
            .collect();
        Self {
            plan,
            lane_cpu_nanos,
            instance_queue_depths,
            total_messages: AtomicU64::new(0),
            cross_lane_messages: AtomicU64::new(0),
        }
    }

    pub(super) fn record_invocation(
        &self,
        observing_lane: &ExecutionLaneId,
        caller: &str,
        provider: &str,
    ) {
        let Some(caller_lane) = self
            .plan
            .module_instance(caller)
            .map(|instance| instance.execution_lane())
        else {
            return;
        };
        if caller_lane != observing_lane {
            return;
        }
        let Some(provider_lane) = self
            .plan
            .module_instance(provider)
            .map(|instance| instance.execution_lane())
        else {
            return;
        };
        self.total_messages.fetch_add(1, Ordering::Relaxed);
        if caller_lane != provider_lane {
            self.cross_lane_messages.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(super) fn publish_lane(&self, lane: &ExecutionLaneId, app: &NativeApp, cpu_time: Duration) {
        if let Some(cpu_nanos) = self.lane_cpu_nanos.get(lane.as_str()) {
            cpu_nanos.store(duration_nanos(cpu_time), Ordering::Relaxed);
        }
        for (instance, depth) in app.instance_queue_depths() {
            if let Some(queue_depth) = self.instance_queue_depths.get(&instance) {
                queue_depth.store(u64::try_from(depth).unwrap_or(u64::MAX), Ordering::Relaxed);
            }
        }
    }

    pub(super) fn snapshot(&self) -> LaneDiagnosticsSnapshot {
        LaneDiagnosticsSnapshot {
            lane_cpu_time: self
                .lane_cpu_nanos
                .iter()
                .map(|(lane, nanos)| {
                    (
                        lane.clone(),
                        Duration::from_nanos(nanos.load(Ordering::Relaxed)),
                    )
                })
                .collect(),
            instance_queue_depths: self
                .instance_queue_depths
                .iter()
                .map(|(instance, depth)| {
                    (
                        instance.clone(),
                        usize::try_from(depth.load(Ordering::Relaxed)).unwrap_or(usize::MAX),
                    )
                })
                .collect(),
            total_messages: self.total_messages.load(Ordering::Relaxed),
            cross_lane_messages: self.cross_lane_messages.load(Ordering::Relaxed),
        }
    }
}

fn duration_nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// One immutable structural snapshot used to evaluate Plan placement.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LaneDiagnosticsSnapshot {
    lane_cpu_time: BTreeMap<String, Duration>,
    instance_queue_depths: BTreeMap<String, usize>,
    total_messages: u64,
    cross_lane_messages: u64,
}

impl LaneDiagnosticsSnapshot {
    /// Returns CPU time consumed by each lane's owner thread.
    pub fn lane_cpu_time(&self) -> &BTreeMap<String, Duration> {
        &self.lane_cpu_time
    }

    /// Returns the latest bounded request queue depth for one Module Instance.
    pub fn instance_queue_depth(&self, instance: &str) -> Option<usize> {
        self.instance_queue_depths.get(instance).copied()
    }

    /// Returns the number of observed App request messages.
    pub const fn total_messages(&self) -> u64 {
        self.total_messages
    }

    /// Returns the number of observed messages whose binding crosses lanes.
    pub const fn cross_lane_messages(&self) -> u64 {
        self.cross_lane_messages
    }

    /// Returns the share of observed request messages whose binding crosses lanes.
    pub fn cross_lane_message_share(&self) -> f64 {
        if self.total_messages == 0 {
            0.0
        } else {
            self.cross_lane_messages as f64 / self.total_messages as f64
        }
    }
}
