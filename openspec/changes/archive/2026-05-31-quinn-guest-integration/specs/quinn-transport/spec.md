## ADDED Requirements

### Requirement: AsyncUdpSocket Implementation
`selium-guest` SHALL implement `quinn::AsyncUdpSocket` for a Quinn-compatible wrapper around `UdpSocket`, enabling Quinn to drive datagram receive operations over the shared-memory recv channel.

#### Scenario: Quinn polls for received datagrams
- **WHEN** Quinn's `EndpointDriver` calls `poll_recv` on the socket and a frame is available in the recv channel
- **THEN** the implementation SHALL copy the payload into the provided buffer and populate the `RecvMeta` with the source address and length

#### Scenario: Quinn polls with empty recv channel
- **WHEN** Quinn's `EndpointDriver` calls `poll_recv` on the socket and the recv channel is empty
- **THEN** the implementation SHALL start a `SignalWait` hostcall on the recv signal and return `Poll::Pending`

### Requirement: UdpSender Implementation
`selium-guest` SHALL implement `quinn::UdpSender` that writes framed datagrams to the shared-memory send channel.

#### Scenario: Quinn sends a datagram
- **WHEN** Quinn calls `poll_send` with a `Transmit` containing a destination address and payload
- **THEN** the implementation SHALL encode the destination and payload into a frame and write it to the send channel

#### Scenario: Quinn sends with full send channel
- **WHEN** Quinn calls `poll_send` and the send channel is full
- **THEN** the implementation SHALL start a `SignalWait` hostcall on the send signal and return `Poll::Pending`

### Requirement: Runtime Implementation
`selium-guest` SHALL implement `quinn::Runtime` to bridge Quinn's I/O driver with the guest's cooperative async runtime.

#### Scenario: Quinn spawns the endpoint driver
- **WHEN** `quinn::Endpoint::new_with_abstract_socket` calls `runtime.spawn(future)`
- **THEN** the implementation SHALL spawn the future onto the guest's background task runner so it is polled by `poll_reactor()`

#### Scenario: Quinn queries the current time
- **WHEN** Quinn calls `runtime.now()`
- **THEN** the implementation SHALL return `std::time::Instant::now()`

### Requirement: AsyncTimer Implementation
`selium-guest` SHALL implement `quinn::AsyncTimer` to provide deadline-based wakeups for Quinn's timeout management.

#### Scenario: Quinn timer reaches deadline
- **WHEN** Quinn polls a timer whose deadline has passed
- **THEN** `poll` SHALL return `Poll::Ready(())`

#### Scenario: Quinn timer not yet expired
- **WHEN** Quinn polls a timer whose deadline has not yet passed
- **THEN** `poll` SHALL arrange for a future wakeup (e.g. via an OS thread that sleeps and wakes the task) and return `Poll::Pending`

### Requirement: Quinn-Compatible Socket Conversion
`UdpSocket` SHALL provide a method to convert itself into the Quinn-compatible wrapper type, transferring ownership of the channel handles.

#### Scenario: Guest converts UdpSocket for Quinn use
- **WHEN** guest code calls `udp_socket.into_quinn_socket()` with the `quinn` feature enabled
- **THEN** the method SHALL return a `QuinnUdpSocket` (or equivalent) that implements `quinn::AsyncUdpSocket` and shares the underlying channel state via `Arc`

### Requirement: Endpoint Construction
The Quinn integration types SHALL be usable with `quinn::Endpoint::new_with_abstract_socket` to create a functional QUIC endpoint.

#### Scenario: Guest constructs a QUIC client endpoint
- **WHEN** guest code constructs a `quinn::Endpoint` using `new_with_abstract_socket` with the Quinn wrapper and runtime
- **THEN** the endpoint SHALL be able to initiate QUIC connections and exchange data
