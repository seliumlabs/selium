//! Tier-1 discovery registration for the runtime.
//!
//! The runtime publishes every allocated resource as a volatile event on the
//! runtime→discovery pub/sub feed using the typed `sel://<tenant>/<type>/<id>`
//! schema. This module provides the URI generation logic; durable registration
//! state lives in the discovery guest, not here.

use selium_abi::{ProcessId, ResourceClass, uri};

/// Generates the tier-1 registration URI for a process node:
/// `sel://<tenant>/proc/<id>`.
pub fn process_registration_uri(tenant: &str, process_id: ProcessId) -> String {
    typed_registration_uri(tenant, ResourceClass::Process, process_id)
}

/// Generates the tier-1 registration URI for a host connection queue created
/// by `HostQueueCreate`: `sel://<tenant>/queue/<id>`. Queues are first-class
/// resources so guests can register routes whose target is a listener queue
/// and still pass discovery's ownership validation.
pub fn queue_registration_uri(tenant: &str, queue_id: u64) -> String {
    typed_registration_uri(tenant, ResourceClass::HostQueue, queue_id)
}

/// Generates the tier-1 registration URI for an allocated region:
/// `sel://<tenant>/region/<id>`.
pub fn region_registration_uri(tenant: &str, region_id: u64) -> String {
    typed_registration_uri(tenant, ResourceClass::SharedRegion, region_id)
}

/// Generates the tier-1 registration URI for a typed resource:
/// `sel://<tenant>/<type>/<id>` with a singular type segment.
pub fn typed_registration_uri(tenant: &str, class: ResourceClass, id: u64) -> String {
    uri::resource_uri(tenant, class, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_registration_uri_is_typed_and_tenant_scoped() {
        assert_eq!(region_registration_uri("acme", 7), "sel://acme/region/7");
    }

    #[test]
    fn region_registration_uri_supports_root_tenant() {
        assert_eq!(region_registration_uri("", 7), "sel:///region/7");
    }

    #[test]
    fn queue_registration_uri_is_typed() {
        assert_eq!(queue_registration_uri("acme", 7), "sel://acme/queue/7");
    }

    #[test]
    fn process_registration_uri_is_typed() {
        assert_eq!(process_registration_uri("acme", 42), "sel://acme/proc/42");
    }

    #[test]
    fn typed_registration_uri_covers_every_class_segment() {
        for class in [
            ResourceClass::SharedRegion,
            ResourceClass::SharedMapping,
            ResourceClass::Signal,
            ResourceClass::TcpListener,
            ResourceClass::TcpStream,
            ResourceClass::UdpSocket,
            ResourceClass::DurableLog,
            ResourceClass::BlobStore,
            ResourceClass::Process,
            ResourceClass::ActivityLog,
            ResourceClass::MeteringStream,
            ResourceClass::GuestLog,
            ResourceClass::HostQueue,
        ] {
            let segment = class.uri_segment();
            let uri = typed_registration_uri("acme", class, 1);
            assert_eq!(uri, format!("sel://acme/{segment}/1"));
        }
    }
}
