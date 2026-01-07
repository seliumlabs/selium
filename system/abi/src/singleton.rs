//! Singleton dependency identifiers and hostcall payloads.

use rkyv::{Archive, Deserialize, Serialize};

use crate::GuestResourceId;

/// Stable identifier for a singleton dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct DependencyId(pub [u8; 16]);

impl DependencyId {
    /// Return the raw byte representation of the identifier.
    pub const fn bytes(self) -> [u8; 16] {
        self.0
    }
}

/// Payload used to register a singleton dependency in the host registry.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct SingletonRegister {
    /// Dependency identifier.
    pub id: DependencyId,
    /// Shared handle to the resource that should back this singleton.
    pub resource: GuestResourceId,
}

/// Payload used to look up a singleton dependency from the host registry.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct SingletonLookup {
    /// Dependency identifier.
    pub id: DependencyId,
}
