# Spec: Discovery Bootstrap

## ADDED Requirements

### Requirement: Discovery-enabled bootstrap

`Runtime::bootstrap_system_guests` with `start_discovery: true` SHALL create
the Tier-1 discovery feed ring and RPC listener, inject the feed region id
and listener handle into the discovery guest's entrypoint arguments using
the tagged `WasmValue` serialisation, inject the listener handle into other
guests with empty argument lists, and gate each guest's readiness on its
own `mark_ready()` signal.

#### Scenario: Discovery guest boots and reports ready

- **WHEN** the runtime bootstraps a configuration containing the discovery
  guest with `start_discovery: true`
- **THEN** the discovery guest receives decodable feed and handle
  arguments, attaches both resources, calls `mark_ready()`, and the
  bootstrap completes without readiness timeout

#### Scenario: Application guest receives a discovery handle

- **WHEN** an application guest with an empty argument list is bootstrapped
  in the same configuration
- **THEN** its entrypoint receives the discovery listener handle as its
  first argument, decodable by `decode_wasm_arguments`, and it can build
  `Context::from_raw` successfully

### Requirement: Tier-1 registration flow

While discovery is running, the runtime SHALL publish register/revoke
events for region allocation, region free, and process exit onto the feed
ring, and the discovery guest SHALL apply them to its registration store.

#### Scenario: Region allocation becomes resolvable

- **WHEN** a process allocates a shared region while discovery is running
- **THEN** a `sel://process/<pid>/regions/<id>` registration is published
  on the feed and becomes resolvable through discovery lookup

#### Scenario: Process exit revokes registrations

- **WHEN** a process exits after allocating regions
- **THEN** the runtime publishes revocation events and lookups for that
  process's Tier-1 URIs stop resolving

### Requirement: Tier-2 guest registration

The discovery guest SHALL accept `Register`/`Resolve`/`Revoke` RPC requests
over the shm rendezvous and SHALL reject Tier-2 registrations for resources
the requesting process does not own.

#### Scenario: Owned resource registration succeeds

- **WHEN** a guest requests Tier-2 registration of a URI mapped to a
  resource id it owns
- **THEN** discovery stores the mapping and responds `Registered`, and the
  URI resolves on subsequent lookups

#### Scenario: Unowned resource registration is forbidden

- **WHEN** a guest requests Tier-2 registration of a URI mapped to a
  resource id owned by a different process
- **THEN** discovery responds `Forbidden` and stores nothing
