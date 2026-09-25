## MODIFIED Requirements

### Requirement: Discovery-Based Route Resolution
The connector SHALL resolve the serving channel for a request via discovery using the unified addressing resolver: the request Host SHALL resolve to a tenant via the domain-to-tenant table (or the synthetic tenant label), and the request path SHALL then resolve within that tenant's namespace. The connector SHALL NOT hold a static routing table.

#### Scenario: Request routed to registered guest
- **WHEN** a request arrives whose Host derives a tenant and whose path resolves to a registered route
- **THEN** the connector SHALL forward the typed request on the resolved channel

#### Scenario: Request routed via a registered domain
- **WHEN** a request arrives for `bridge.example.com/healthz` and `example.com -> acme` is provisioned
- **THEN** the connector SHALL resolve the host to tenant `acme` and route the path within `acme`'s namespace

#### Scenario: Apex request routed to the root service
- **WHEN** a request arrives for the bare domain `example.com` (with or without a request path), `example.com -> acme` is provisioned, and `acme` designates a root service
- **THEN** the connector SHALL route the request to the root service, which handles the request path itself

#### Scenario: No route registered
- **WHEN** no registration matches the resolved tenant/path
- **THEN** the connector SHALL respond with a typed 404-equivalent response without contacting any app guest
