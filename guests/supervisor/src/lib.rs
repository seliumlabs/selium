//! Supervisor system guest.

use std::collections::BTreeMap;

use selium_guest::{
    entrypoint,
    pattern_interface,
};

pub const PROCESS_HEALTH_TABLE: &str = "selium.supervisor.health";
pub const RECOVERY_INTENT_TOPIC: &str = "selium.supervisor.recovery";
pub const RESTART_POLICY_LOG: &str = "selium.supervisor.restart-policy";
pub const RUNTIME_ACTIVITY_CURSOR: &str = "selium.supervisor.activity-cursor";

#[pattern_interface]
pub trait SupervisorControl {
    fn observe_process(process_id: u64);
    fn health(process_id: u64);
    fn recovery_intents();
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartPolicy {
    Never,
    Always,
    OnFailure,
    Backoff {
        initial_delay_ms: u64,
        max_delay_ms: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureClass {
    ExpectedStop,
    Unhealthy,
    Crashed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    Starting,
    Healthy,
    Failed(FailureClass),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedProcess {
    pub process_id: u64,
    pub workload_id: String,
    pub policy: RestartPolicy,
    pub failures: u32,
    pub status: HealthStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryIntent {
    Restart { workload_id: String, after_ms: u64 },
    Reschedule { workload_id: String, reason: String },
}

#[derive(Debug, Clone, Default)]
pub struct SupervisorState {
    processes: BTreeMap<u64, ManagedProcess>,
    recovery: Vec<RecoveryIntent>,
}

impl SupervisorState {
    pub fn track(&mut self, process: ManagedProcess) {
        self.processes.insert(process.process_id, process);
    }

    pub fn mark_healthy(&mut self, process_id: u64) -> bool {
        let Some(process) = self.processes.get_mut(&process_id) else {
            return false;
        };
        process.status = HealthStatus::Healthy;
        true
    }

    pub fn classify_failure(
        &mut self,
        process_id: u64,
        failure: FailureClass,
    ) -> Option<RecoveryIntent> {
        let process = self.processes.get_mut(&process_id)?;
        process.failures = process.failures.saturating_add(1);
        process.status = HealthStatus::Failed(failure.clone());

        let intent = match (&process.policy, failure) {
            (RestartPolicy::Never, _) | (RestartPolicy::OnFailure, FailureClass::ExpectedStop) => {
                return None;
            }
            (RestartPolicy::Always | RestartPolicy::OnFailure, _) => RecoveryIntent::Restart {
                workload_id: process.workload_id.clone(),
                after_ms: 0,
            },
            (
                RestartPolicy::Backoff {
                    initial_delay_ms,
                    max_delay_ms,
                },
                _,
            ) => RecoveryIntent::Restart {
                workload_id: process.workload_id.clone(),
                after_ms: backoff_delay(*initial_delay_ms, *max_delay_ms, process.failures),
            },
        };
        self.recovery.push(intent.clone());
        Some(intent)
    }

    pub fn request_reschedule(&mut self, process_id: u64, reason: impl Into<String>) -> bool {
        let Some(process) = self.processes.get(&process_id) else {
            return false;
        };
        self.recovery.push(RecoveryIntent::Reschedule {
            workload_id: process.workload_id.clone(),
            reason: reason.into(),
        });
        true
    }

    pub fn recovery_intents(&self) -> &[RecoveryIntent] {
        &self.recovery
    }
}

pub fn interface_metadata() -> selium_guest::InterfaceMetadata {
    supervisorcontrol_pattern_metadata()
}

fn backoff_delay(initial_delay_ms: u64, max_delay_ms: u64, failures: u32) -> u64 {
    let mut delay = initial_delay_ms;
    for _ in 1..failures {
        delay = delay.saturating_mul(2).min(max_delay_ms);
    }
    delay.min(max_delay_ms)
}

#[entrypoint]
async fn supervisor_main() {
    drop(selium_guest::log::init());
    selium_guest::info!(guest = "selium-supervisor", "system guest booting");
    selium_guest::mark_ready();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(policy: RestartPolicy) -> ManagedProcess {
        ManagedProcess {
            process_id: 42,
            workload_id: "api".to_string(),
            policy,
            failures: 0,
            status: HealthStatus::Starting,
        }
    }

    #[test]
    fn tracks_process_health() {
        let mut state = SupervisorState::default();
        state.track(process(RestartPolicy::OnFailure));

        assert!(state.mark_healthy(42));
    }

    #[test]
    fn evaluates_restart_policy() {
        let mut state = SupervisorState::default();
        state.track(process(RestartPolicy::OnFailure));

        let intent = state.classify_failure(42, FailureClass::Crashed);

        assert_eq!(
            intent,
            Some(RecoveryIntent::Restart {
                workload_id: "api".to_string(),
                after_ms: 0,
            })
        );
    }

    #[test]
    fn applies_backoff_policy() {
        let mut state = SupervisorState::default();
        state.track(process(RestartPolicy::Backoff {
            initial_delay_ms: 100,
            max_delay_ms: 250,
        }));

        state.classify_failure(42, FailureClass::Crashed);
        let intent = state.classify_failure(42, FailureClass::Crashed);

        assert_eq!(
            intent,
            Some(RecoveryIntent::Restart {
                workload_id: "api".to_string(),
                after_ms: 200,
            })
        );
    }
}
