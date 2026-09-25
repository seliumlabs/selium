# Tasks: QUIC Connector

## 1. Dependency and workspace setup

- [x] 1.1 Add `quinn` to `[workspace.dependencies]` with `default-features = false` and `rustls-ring`; verify `cargo metadata` resolves the dependency cleanly on the default target
- [x] 1.2 Delete `crates/quic` (`selium-quic`); verify `cargo check --workspace` is unaffected and a repo-wide search finds `selium-quic`/`selium_quic` only in frozen or archived locations (never an active member)

## 2. Quinn-on-wasm32 spike

- [x] 2.1 Create `guests/connector-quic` (`selium-connector-quic`, cdylib guest) with a `#[entrypoint]` that binds a `UdpSocket`; verify the crate compiles for the guest target
- [x] 2.2 Implement a `quinn::AsyncUdpSocket` adapter over `selium_guest::net::UdpSocket` (map `Transmit`/`RecvMeta` ↔ `Datagram`) and a `quinn::Runtime` over the guest executor, `sleep` hostcall, and clock hostcalls; verify both types satisfy quinn's trait bounds via `cargo check -p selium-connector-quic`
- [x] 2.3 Build a quinn server endpoint with `Endpoint::new_with_abstract_socket` and complete a TLS 1.3 handshake against a host-side quinn test client; verify the handshake succeeds (test asserts `incoming.await` yields a connection)

## 3. Shared byte-channel substrate

- [x] 3.1 Factor `TcpStream`'s two-ring attach logic into a shared internal byte-stream helper in `selium-guest` (or `selium-shm`); verify existing `TcpStream` tests still pass via `cargo test -p selium-guest`
- [x] 3.2 Add a connector-side byte-stream-over-region type that allocates a two-ring region and attaches it as `AsyncRead`/`AsyncWrite` using the shared helper; verify with a unit test that writes bytes on one handle and reads them on a peer handle

## 4. App-guest serve API

- [x] 4.1 Add `selium-guest::net::quic` with the `sel-quic` scheme constant, `QuicServe::bind(&mut ctx, "sel-quic://<name>")` (discovery registration), and `accept()` yielding a `QuicStream` byte handle; verify a unit test registers the URI, accepts a delivered region, and round-trips bytes
- [x] 4.2 Document the zero-`Network`-grant capability model and `ExplicitResource` per-stream hygiene in the module docs; verify the doc builds without warnings via `cargo doc -p selium-guest --no-deps`

## 5. Connector routing and relay

- [x] 5.1 Implement SNI-based route resolution (`sel-quic://<name>` discovery lookup with cache and evict-on-attach-failure, mirroring the HTTP `RouteResolver`); verify unit tests for hit/miss/evict semantics
- [x] 5.2 Resolve the serving guest from `Connection::handshake_data()` SNI and refuse the handshake when SNI is absent or unregistered; verify a test asserts the refused handshake never contacts any app guest
- [x] 5.3 Allocate a per-stream two-ring region per accepted bidirectional stream, grant it `ExplicitResource` to {connector, guest}, and deliver it via `ResourceSender` to the resolved guest queue; verify a substrate test observes one delivered stream per QUIC stream
- [x] 5.4 Implement the bidirectional relay pumps (QUIC `RecvStream` ↔ ring, ring ↔ `SendStream`) such that a full ring stops polling the `RecvStream` (quinn flow control pushes back) with no unbounded buffering; verify pipeline tests show byte-identical, ordered delivery
- [x] 5.5 Surface stream lifecycle: client FIN → guest sees EOF/closed channel; guest close → connector `SendStream::finish()` (or `reset()` on error); verify tests for both directions

## 6. Integration verification

- [x] 6.1 Golden path: external quinn client → connector handshake + stream → per-stream channel → app guest read/write → bytes returned (verify `cargo test -p selium-connector-quic` plus a `selium-runtime` substrate test for the channel handoff)
- [x] 6.2 Backpressure honesty: a slow app guest makes the connector pause the stream with no bytes lost, and a slow client parks the guest's ring writes; verify with connector pipeline tests plus a `selium-runtime` channel test
- [x] 6.3 Capability and isolation: an app guest with only channel-attach grants serves successfully; streams on one connection do not cross; an ungranted third party's attach is denied; verify with `selium-runtime` grants tests
- [x] 6.4 CI: unknown/absent SNI is refused at the handshake with no app guest contacted; concurrent streams on one connection relay independently; verify via `cargo test -p selium-connector-quic`
