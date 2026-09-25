//! Cluster system guest.

use std::collections::BTreeMap;

use selium_guest::{
    entrypoint,
    pattern_interface,
};

pub const CLUSTER_COORDINATION_EXCHANGE: &str = "selium.cluster.coordination";
pub const EXTERNAL_BOOTSTRAP_TOPIC: &str = "selium.cluster.external-bootstrap";
pub const HOST_LOAD_TABLE: &str = "selium.cluster.host-load";
pub const HOST_MEMBERSHIP_TABLE: &str = "selium.cluster.hosts";

#[pattern_interface]
pub trait ClusterControl {
    fn upsert_host(record: HostRecord);
    fn remove_host(host_id: String);
    fn update_host_load(host_id: String, load: HostLoad);
    fn bootstrap_addresses();
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostLoad {
    pub cpu_millis: u32,
    pub memory_bytes: u64,
    pub active_processes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostAvailability {
    Joining,
    Available,
    Draining,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRecord {
    pub id: String,
    pub address: String,
    pub load: HostLoad,
    pub availability: HostAvailability,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeferredClusterWork {
    DnsTxtPublishing,
    QuicMtlsBridge,
}

#[derive(Debug, Clone, Default)]
pub struct ClusterState {
    hosts: BTreeMap<String, HostRecord>,
    bootstrap_addresses: Vec<String>,
    peers: Vec<String>,
}

impl HostLoad {
    pub fn available() -> Self {
        Self {
            cpu_millis: 0,
            memory_bytes: 0,
            active_processes: 0,
        }
    }
}

impl ClusterState {
    pub fn bootstrap_single_host(&mut self, host: HostRecord) {
        self.bootstrap_addresses = vec![host.address.clone()];
        self.hosts.insert(host.id.clone(), host);
    }

    pub fn upsert_host(&mut self, host: HostRecord) {
        self.hosts.insert(host.id.clone(), host);
    }

    pub fn remove_host(&mut self, host_id: &str) -> Option<HostRecord> {
        self.hosts.remove(host_id)
    }

    pub fn update_host_load(&mut self, host_id: &str, load: HostLoad) -> bool {
        let Some(host) = self.hosts.get_mut(host_id) else {
            return false;
        };
        host.load = load;
        true
    }

    pub fn host_load_view(&self) -> Vec<HostRecord> {
        self.hosts.values().cloned().collect()
    }

    pub fn add_peer(&mut self, address: impl Into<String>) {
        self.peers.push(address.into());
    }

    pub fn peers(&self) -> &[String] {
        &self.peers
    }

    pub fn bootstrap_addresses(&self) -> &[String] {
        &self.bootstrap_addresses
    }
}

pub fn deferred_day1_work() -> Vec<DeferredClusterWork> {
    vec![
        DeferredClusterWork::DnsTxtPublishing,
        DeferredClusterWork::QuicMtlsBridge,
    ]
}

pub fn interface_metadata() -> selium_guest::InterfaceMetadata {
    clustercontrol_pattern_metadata()
}

#[entrypoint]
async fn cluster_main() {
    drop(selium_guest::log::init());
    selium_guest::info!(guest = "selium-cluster", "system guest booting");
    selium_guest::mark_ready();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(id: &str, address: &str) -> HostRecord {
        HostRecord {
            id: id.to_string(),
            address: address.to_string(),
            load: HostLoad::available(),
            availability: HostAvailability::Available,
        }
    }

    #[test]
    fn bootstraps_single_host_state() {
        let mut state = ClusterState::default();

        state.bootstrap_single_host(host("host-a", "127.0.0.1:7000"));

        assert_eq!(state.host_load_view().len(), 1);
        assert_eq!(state.bootstrap_addresses(), &["127.0.0.1:7000".to_string()]);
    }

    #[test]
    fn projects_host_load_for_consumers() {
        let mut state = ClusterState::default();
        state.upsert_host(host("host-a", "127.0.0.1:7000"));

        let updated = state.update_host_load(
            "host-a",
            HostLoad {
                cpu_millis: 500,
                memory_bytes: 1024,
                active_processes: 2,
            },
        );

        assert!(updated);
        assert_eq!(state.host_load_view()[0].load.cpu_millis, 500);
    }

    #[test]
    fn records_deferred_day1_boundaries() {
        assert_eq!(deferred_day1_work().len(), 2);
    }

    #[test]
    fn records_cross_host_coordination_peers() {
        let mut state = ClusterState::default();

        state.add_peer("10.0.0.2:7000");

        assert_eq!(state.peers(), &["10.0.0.2:7000".to_string()]);
    }
}
