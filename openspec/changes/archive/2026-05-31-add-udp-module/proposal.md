## Why

The guest SDK and kernel currently only support TCP networking (`TcpStream`, `TcpListener`). Adding UDP support opens the door to QUIC (via Quinn), DNS, custom datagram protocols, and other UDP-based communication — none of which are possible with the current connection-oriented channel model.

## What Changes

- **New ABI hostcall**: `HostcallRequest::UdpBind` for binding a UDP socket on the host side
- **New ABI resource class**: `ResourceClass::UdpSocket` for capability-gated access to UDP resources
- **New guest module**: `selium_guest::net::udp::UdpSocket` — a shared-memory backed datagram socket analogous to `TcpStream` but for connectionless, datagram-oriented I/O
- **New kernel component**: UDP proxy that bridges a real OS UDP socket ↔ guest shared-memory channels, running in dedicated OS threads (same pattern as TCP)
- **Updated runtime hostcall dispatcher**: New match arm for `UdpBind` with capability checks and resource tracking
- **Updated process teardown**: Close/cleanup UDP socket state on guest exit
- **No Quinn integration**: This change is UDP infrastructure only — Quinn guest support is a separate follow-up

## Capabilities

### New Capabilities
- `udp-transport`: covers the full UDP lifecycle — bind, receive datagrams, send datagrams, and close. This includes the ABI contract, guest SDK type, kernel proxy, and runtime integration.

### Modified Capabilities
- `selium-abi`: requires new `UdpBind` hostcall variant, new `UdpSocket` resource class, and datagram-oriented hostcall output types
- `selium-guest`: requires exposing the new `net::udp` module and `UdpSocket` type in the public API
- `selium-kernel`: requires new UDP proxy runtime and associated state types, plus clean-up in the process lifecycle

## Impact

| Area | Impact |
|------|--------|
| `selium-abi` | New `UdpBind` variant in `HostcallRequest`; new `UdpSocket` variant in `ResourceClass`; possibly new `HostcallOutput` variant if `SharedRegion` is insufficient for carrying metadata (source addr, ECN, etc.) |
| `selium-guest` | New `src/net/udp.rs` module with `UdpSocket` type; new `pub use` in `lib.rs`; new tests |
| `selium-kernel` | New `UdpSocketState` in `state.rs`; new UDP proxy functions in `network_runtime.rs` (bind, proxy recv, proxy send, close) |
| `selium-runtime` | New `UdpBind` match arm in hostcall dispatcher; new resource class handling in capability checks and process teardown |
| Axum | Unaffected |
| Quinn | No changes — Quinn integration is deferred |
