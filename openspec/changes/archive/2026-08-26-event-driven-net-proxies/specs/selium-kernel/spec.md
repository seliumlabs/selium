## ADDED Requirements

### Requirement: Event-Driven Network Poller
The kernel SHALL provide a single OS-event-port poller thread (epoll/
kqueue/IOCP via mio) that owns all network proxy socket readiness.
Proxy sockets — TCP streams for inbound reads, TCP listeners for
accepts, and UDP sockets for receives — SHALL be registered by shared
region id token. A readable event SHALL pump the available bytes or
datagrams into the corresponding inbound ring, advance the ring's
generation, and invoke the runtime's generation-advance callback so
registered guest tasks are woken via the mailbox.

#### Scenario: Socket data reaches the inbound ring without polling
- **WHEN** a proxy socket becomes readable while no guest is executing
- **THEN** the poller thread SHALL deliver the data to the inbound ring
  and bridge to the guest wake path without any sleep-based retry loop

#### Scenario: Accept is event-driven
- **WHEN** a registered listener becomes readable
- **THEN** the poller SHALL accept the connection, create its stream
  region, and enqueue it on the host queue from within the poller

### Requirement: Poller Registration Hygiene
The kernel poller SHALL deregister a socket and release its entry when
the socket reaches EOF, fails with a fatal error, or its running flag
is cleared. Accept callbacks and generation-advance callbacks SHALL run
without holding poller registry locks, so that callbacks may register
new sockets (including re-entrantly from a guest reactor executed on
the poller thread).

#### Scenario: Closed connections do not leak registrations
- **WHEN** a proxied stream observes EOF or a fatal read error
- **THEN** its fd SHALL be removed from the poller registry and its
  entry SHALL be dropped

#### Scenario: Callbacks may register new sockets
- **WHEN** an accept callback or generation-advance callback runs code
  that registers another socket with the same poller
- **THEN** registration SHALL complete without deadlock
