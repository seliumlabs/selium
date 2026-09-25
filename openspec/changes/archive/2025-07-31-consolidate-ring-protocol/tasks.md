# Tasks: Consolidate the Ring Protocol

## 1. Shared layout module

- [x] 1.1 Create `selium-shm::layout`: offset constants, slot encode/decode, `reserve_tail_next`, wrap-around split helpers (moved from `region.rs`/`cursor.rs`; re-export for compatibility).
- [x] 1.2 Move `FrameHeader` from `selium-wire::frame` into the layout module; re-export from `selium-wire` to keep the public API stable.
- [x] 1.3 Implement `RingReader`/`RingWriter` primitives generic over `MappingBackend` (read_frame/write_frame/reserve/slot ops), used by `RingBuf` internally.

## 2. Multi-memory header

- [x] 2.1 Move the multi-memory header definition (magic, count, entry table) into `selium-memory` with `write_header`/`parse_header`.
- [x] 2.2 `shm/rpc.rs` and `kernel/network_runtime.rs` use the shared header; delete local constants and parsers.

## 3. Kernel adoption

- [x] 3.1 Expose a kernel-usable backend (`KernelBackend` shared from runtime or moved to a place kernel can depend on without cycles).
- [x] 3.2 Rewrite `network_runtime.rs` proxies (inbound/outbound, UDP) on `RingReader`/`RingWriter`; delete the bespoke frame/reserve/slot code (~600 lines).
- [x] 3.3 Rewrite `drain_log_channel` on the shared frame reader with caller position; delete the bespoke parser.
- [x] 3.4 Kernel reader slots allocate via the shared `reader_slot_counter`; delete the hard-coded slot 0.

## 4. Atomicity contract

- [x] 4.1 Document the single-writer-domain rule in `selium-shm` docs and `AGENTS.md`; add debug assertions where a domain tag is available.

## 5. Gates

- [x] 5.1 Port kernel network tests to the shared primitives (echo and EOF assertions preserved).
- [x] 5.2 Gates: fmt, clippy `-D warnings`, full suite, wasm32 builds, spine test green.