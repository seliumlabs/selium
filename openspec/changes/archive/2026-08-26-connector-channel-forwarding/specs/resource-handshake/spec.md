## ADDED Requirements

### Requirement: Queue Attach Authorisation

`HostQueueAttach` against a host queue owned by another process SHALL
succeed only when the caller holds an `ExplicitResource` grant naming
that queue, or the caller obtained the queue's shared id through a
successful discovery resolution performed by the caller. Attach attempts
without either basis SHALL be denied with a capability error. A process
SHALL always be permitted to attach to queues it created itself.

#### Scenario: Ungranted attach denied

- **WHEN** a process attempts `HostQueueAttach` on a queue owned by
  another process without a grant naming it and without having resolved
  it via discovery
- **THEN** the hostcall is denied with a capability error

#### Scenario: Discovery-resolved attach permitted

- **WHEN** a connector resolves a URI subtree via discovery and uses the
  returned queue id to attach as a sender
- **THEN** the attach succeeds because the descriptor was obtained
  through discovery resolution

#### Scenario: Owner attach always permitted

- **WHEN** a process attaches to a host queue it created itself
- **THEN** the attach succeeds without additional grants
