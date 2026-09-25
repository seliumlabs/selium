# Tasks: Connector Channel Forwarding

## 1. Runtime: resolve records and queue-attach authorisation

- [x] 1.1 Record queue ids returned to each process by successful discovery Resolve hostcalls (HashSet on the process authority record) and verify with a unit test that resolve/attach of a foreign queue succeeds after resolve (`cargo test -p selium-runtime`)
- [x] 1.2 Add ownership/grant/resolve-basis check to the `HostQueueAttach` hostcall handler; deny with capability error otherwise; verify ungranted, unresolved attach is denied in a runtime test (`cargo test -p selium-runtime`)

## 2. Guest SDK: expose the composed flow

- [x] 2.1 Verify/extend `selium_shm::rpc::connect` + `ResourceSender::attach` path so a connector can go discovery-resolve → queue attach → region alloc → rendezvous against a real listener; wire an integration test over the mock/kernel layer (`cargo test -p selium-shm`)
- [x] 2.2 Replace the placeholder forward path in `handle_tls_connection` with `RpcClient<HttpRequest, HttpResponse>` session establishment per resolved route; verify compilation and unit tests still pass (`cargo test -p selium-connector-http`)

## 3. Connector: lifecycle and stale-route handling

- [x] 3.1 Tie region-pair lifetime to external connection teardown: free regions on socket EOF or serving-guest `ConnectionClosed`; verify with a connector unit test using mock transports
- [x] 3.2 On queue-attach failure: evict the route-cache entry, emit typed error response for that request only, continue accept loop; unit-test cache eviction + subsequent re-resolution (`cargo test -p selium-connector-http`)
- [x] 3.3 RouteResolver cache invalidation on revocation notification (or on attach-failure as fallback if live notifications are unavailable); verify stale entry is not reused across requests

## 4. Verification

- [x] 4.1 Runtime CI: zero-grant app guest receives region via its listener queue, attaches successfully, serves one typed request/response round trip; third-party guest without queue basis is denied attach (`cargo test -p selium-runtime`)
- [x] 4.2 Runtime CI: concurrent forwarded connections use distinct region pairs and all reclaim on disconnect (no leaked regions in kernel registry at test end)
- [x] 4.3 End-to-end golden path: revoke route mid-run → next request fails loudly with connector error response, no misrouting; following request after re-registration succeeds
