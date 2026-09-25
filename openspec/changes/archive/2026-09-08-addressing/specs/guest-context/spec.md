## ADDED Requirements

### Requirement: Serve-based route registration
`Context` SHALL provide a `serve` method that registers a named route from a resource the guest created itself. The route SHALL derive from a path (for example `["bridge"]`) plus the guest's tenant, producing both the internal path (`sel://<tenant>/bridge`) and the wire name (`bridge.<tenant>`, or `bridge.<owned-domain>` when the domain table maps the tenant). The method SHALL optionally accept a root-service flag for apex aliasing.

#### Scenario: Guest registers its own route
- **WHEN** a guest calls `ctx.serve(Serve { path: ["bridge"], target, default: false }).await`
- **THEN** discovery SHALL store the route resolvable as `sel://<tenant>/bridge` and `bridge.<tenant>`

#### Scenario: Route registration derives wire name from the domain table
- **WHEN** the guest's tenant owns a registered domain and serves a path
- **THEN** the wire name under that domain SHALL also resolve to the route

### Requirement: Root registration capability-gated
Registration in the root/system tenant SHALL be permitted only when the guest holds the corresponding registration capability, replacing the runtime's special-case well-known URI provisioning.

#### Scenario: Root registration requires capability
- **WHEN** a guest without the system-registration capability attempts to register in the root tenant
- **THEN** the request SHALL be forbidden

#### Scenario: Capability-holding guest registers in root
- **WHEN** a guest holding the system-registration capability registers in the root tenant
- **THEN** the registration SHALL be accepted
