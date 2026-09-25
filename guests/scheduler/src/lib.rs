//! Scheduler system guest.

use std::collections::{
    BTreeMap,
    BTreeSet,
};

use selium_guest::{
    entrypoint,
    pattern_interface,
};

pub const DESIRED_WORKLOAD_TABLE: &str = "selium.scheduler.desired-workloads";
pub const PLACEMENT_INTENT_EXCHANGE: &str = "selium.scheduler.placement";
pub const SCHEDULER_STATE_LOG: &str = "selium.scheduler.state";
pub const WORKLOAD_STATUS_TOPIC: &str = "selium.scheduler.status";

#[pattern_interface]
pub trait SchedulerControl {
    fn place(spec: WorkloadSpec);
    fn scale(workload_id: String, replicas: u32);
    fn status(workload_id: String);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPlacementInput {
    pub host_id: String,
    pub available: bool,
    pub free_cpu_millis: u32,
    pub free_memory_bytes: u64,
    pub isolation_keys: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkloadSpec {
    pub workload_id: String,
    pub tenant: String,
    pub cpu_millis: u32,
    pub memory_bytes: u64,
    pub dependencies: Vec<String>,
    pub isolation_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkloadStatus {
    Pending,
    Scheduled,
    Running,
    Stopped,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementDecision {
    pub workload_id: String,
    pub host_id: Option<String>,
    pub accepted: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct SchedulerState {
    desired: BTreeMap<String, PlacementDecision>,
    status: BTreeMap<String, WorkloadStatus>,
}

impl SchedulerState {
    pub fn submit_placement(
        &mut self,
        spec: WorkloadSpec,
        hosts: &[HostPlacementInput],
        resolved_dependencies: &BTreeSet<String>,
    ) -> PlacementDecision {
        if let Some(missing) = spec
            .dependencies
            .iter()
            .find(|dependency| !resolved_dependencies.contains(*dependency))
        {
            return self.reject(
                spec.workload_id,
                format!("dependency not resolved: {missing}"),
            );
        }

        let decision = hosts
            .iter()
            .find(|host| host_satisfies(host, &spec))
            .map_or_else(
                || {
                    self.reject(
                        spec.workload_id.clone(),
                        "no host satisfies placement".to_string(),
                    )
                },
                |host| PlacementDecision {
                    workload_id: spec.workload_id.clone(),
                    host_id: Some(host.host_id.clone()),
                    accepted: true,
                    reason: "placement accepted".to_string(),
                },
            );

        self.desired
            .insert(decision.workload_id.clone(), decision.clone());
        self.status
            .insert(decision.workload_id.clone(), WorkloadStatus::Scheduled);
        decision
    }

    pub fn reconcile_running(&mut self, workload_id: &str) -> Option<WorkloadStatus> {
        if !self.desired.contains_key(workload_id) {
            return None;
        }
        self.status
            .insert(workload_id.to_string(), WorkloadStatus::Running)
    }

    pub fn status(&self, workload_id: &str) -> Option<WorkloadStatus> {
        self.status.get(workload_id).cloned()
    }

    pub fn desired_state(&self) -> Vec<PlacementDecision> {
        self.desired.values().cloned().collect()
    }

    fn reject(&mut self, workload_id: String, reason: String) -> PlacementDecision {
        let decision = PlacementDecision {
            workload_id: workload_id.clone(),
            host_id: None,
            accepted: false,
            reason,
        };
        self.status.insert(
            workload_id.clone(),
            WorkloadStatus::Failed(decision.reason.clone()),
        );
        self.desired.insert(workload_id, decision.clone());
        decision
    }
}

pub fn interface_metadata() -> selium_guest::InterfaceMetadata {
    schedulercontrol_pattern_metadata()
}

fn host_satisfies(host: &HostPlacementInput, spec: &WorkloadSpec) -> bool {
    host.available
        && host.free_cpu_millis >= spec.cpu_millis
        && host.free_memory_bytes >= spec.memory_bytes
        && spec
            .isolation_key
            .as_ref()
            .is_none_or(|key| !host.isolation_keys.contains(key))
}

#[entrypoint]
async fn scheduler_main() {
    drop(selium_guest::log::init());
    selium_guest::info!(guest = "selium-scheduler", "system guest booting");
    selium_guest::mark_ready();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(host_id: &str, cpu: u32, memory: u64) -> HostPlacementInput {
        HostPlacementInput {
            host_id: host_id.to_string(),
            available: true,
            free_cpu_millis: cpu,
            free_memory_bytes: memory,
            isolation_keys: BTreeSet::new(),
        }
    }

    fn spec(workload_id: &str) -> WorkloadSpec {
        WorkloadSpec {
            workload_id: workload_id.to_string(),
            tenant: "tenant-a".to_string(),
            cpu_millis: 250,
            memory_bytes: 512,
            dependencies: Vec::new(),
            isolation_key: None,
        }
    }

    #[test]
    fn places_workload_on_host_with_capacity() {
        let mut state = SchedulerState::default();

        let decision =
            state.submit_placement(spec("api"), &[host("host-a", 500, 1024)], &BTreeSet::new());

        assert!(decision.accepted);
        assert_eq!(decision.host_id, Some("host-a".to_string()));
        assert_eq!(state.status("api"), Some(WorkloadStatus::Scheduled));
    }

    #[test]
    fn rejects_unresolved_dependencies() {
        let mut state = SchedulerState::default();
        let mut workload = spec("api");
        workload.dependencies = vec!["sel://tenant/db".to_string()];

        let decision =
            state.submit_placement(workload, &[host("host-a", 500, 1024)], &BTreeSet::new());

        assert!(!decision.accepted);
        assert!(decision.reason.contains("dependency not resolved"));
    }

    #[test]
    fn publishes_running_status_after_reconcile() {
        let mut state = SchedulerState::default();
        state.submit_placement(spec("api"), &[host("host-a", 500, 1024)], &BTreeSet::new());

        state.reconcile_running("api");

        assert_eq!(state.status("api"), Some(WorkloadStatus::Running));
    }
}
