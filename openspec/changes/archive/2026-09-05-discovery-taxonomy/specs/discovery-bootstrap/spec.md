## MODIFIED Requirements

### Requirement: Tier-1 registration flow
While discovery is running, the runtime SHALL publish register/revoke events for region allocation, region free, and process exit onto the feed ring, and the discovery guest SHALL apply them to its registration store.

#### Scenario: Region allocation becomes resolvable
- **WHEN** a process allocates a shared region while discovery is running
- **THEN** a `sel://<tenant>/region/<id>` registration is published on the feed and becomes resolvable through discovery lookup

#### Scenario: Process exit revokes registrations
- **WHEN** a process exits after allocating regions
- **THEN** the runtime publishes revocation events and lookups for that process's Tier-1 URIs stop resolving
