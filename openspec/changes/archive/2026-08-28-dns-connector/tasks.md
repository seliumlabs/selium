# Tasks: DNS Connector

## 1. Protocol types

- [x] 1.1 Create `selium-proto-dns`: `DnsQuery`, `DnsResponse`, record enums (A/AAAA/CNAME/…), outcome variants (Ok/NxDomain/Timeout/Truncated) via `selium-encoding`
- [x] 1.2 Wire-format codec: schema type ↔ real DNS message (encode query, parse response), with tests against known-good packets

## 2. Connector guest

- [x] 2.1 Create `selium-connector-dns` guest: raw `UdpSocket` bound via grant, upstream resolver address from entrypoint pointer argument (`entrypoint-arguments`)
- [x] 2.2 Accept typed `DnsQuery` on its well-known channel; allocate txid; emit wire query
- [x] 2.3 In-flight map `(txid, resolver addr) → (reply channel, tag)`; demux replies; drop unknown txids
- [x] 2.4 Typed failure mapping: timeout / NXDOMAIN / truncation each produce distinct `DnsResponse` outcomes
- [x] 2.5 Well-known channel provisioned like the discovery listener: the runtime injects the listener queue and registers the well-known URI at provision time (registration is provision-time, per the system-guest channel idiom)

## 3. Guest API

- [x] 3.1 `selium_guest::net::resolve(ctx, name) -> Result<Vec<IpAddr>>`: discovery attach + unary RPC + typed outcome → address list or error

## 4. Verification

- [x] 4.1 CI: fake upstream UDP DNS server (loopback) → guest resolves `example.test` to `127.0.0.1`, then `TcpConnect`s to the literal
- [x] 4.2 CI: NXDOMAIN and timeout surface as distinct typed outcomes (wire codec + connector outcome mapping unit-tested)
- [x] 4.3 CI: a guest without a grant for the connector channel cannot resolve
- [x] 4.4 CI: reply with unknown txid is dropped (no cross-talk between concurrent queries)

## Notes

- 4.1 is the `selium-runtime` `dns_spine` test (fake loopback DNS resolver +
  fake TCP server + `selium-dns-demo`, both WASM guests) and is wired into CI.
- Getting 4.1 green required three runtime fixes, all in this change:
  1. `UdpBind`/`TcpConnect` now also claim their ring regions under
     `SharedRegion` (a guest could not `AttachRegion` its own socket).
  2. Guest ring writes now notify the runtime via a `GenerationAdvance`
     hostcall, so cross-guest `WaitRegister` waiters wake (previously only
     host-side poller writes reached `note_generation_advance`).
  3. The UDP socket and RPC sub-rings now carry their parent `shared_id`
     (`from_mapping_with_id`) so generation waits and bumps route correctly.
