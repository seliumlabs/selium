# Tasks: Guest Network Sockets

## 1. Guest `net` module

- [x] 1.1 Add `selium_guest::net` with `TcpStream`, `TcpListener`, `UdpSocket`, `Datagram`
- [x] 1.2 `TcpStream::connect(addr)`: literals-only validation, `TcpConnect` hostcall, attach region, parse `MultiMemoryHeader`, build `Reader`(sub 0) + `Writer`(sub 1)
- [x] 1.3 Implement `tokio::io::AsyncRead`/`AsyncWrite` for `TcpStream` delegating to `Reader`/`Writer`, registering wakers via `register_generation_wait` (no `wake_by_ref` spin)
- [x] 1.4 Implement `Accept` for `TcpStream`: `IncomingConnection { shared_id }` → attach → stream
- [x] 1.5 `TcpListener::bind(addr)` → `TcpBind` hostcall → `ResourceListener::from_queue`; `accept()` → `Accept::accept` → `TcpStream`
- [x] 1.6 `UdpSocket::bind(addr)` → `UdpBind` hostcall → attach → `poll_send(Datagram)`/`poll_recv() -> Poll<Datagram>` with the binary datagram codec

## 2. Kernel/runtime enforcement

- [x] 2.1 Runtime validates `TcpConnect`/`TcpBind`/`UdpBind` addresses as IP literals (`SocketAddr` parse); reject names with `AbiErrorCode::MalformedPayload`
- [x] 2.2 Replace kernel UDP datagram frame codec with the binary format (`[ver][family][addr][port][payload]`) in both proxy directions
- [x] 2.3 Update kernel UDP proxy tests to the binary format

## 3. Verification (golden path)

- [x] 3.1 CI test: WASM guest binds `127.0.0.1:0`, host connects, guest echoes, guest logs receipt (`cargo test -p selium-runtime`)
- [x] 3.2 CI test: WASM guest `TcpStream::connect("127.0.0.1:…")` to a host helper listener, round-trip bytes
- [x] 3.3 CI test: guest UDP loopback send/recv with binary datagrams
- [x] 3.4 CI test: hostname connect (`"localhost:80"`) is rejected loudly at the hostcall
