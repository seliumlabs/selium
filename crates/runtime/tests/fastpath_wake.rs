//! End-to-end shared-page fast-path test (task 5.1 of the
//! `shared-page-fastpath` change).
//!
//! Deploys the real `selium-net-demo` WASM guest **built with the
//! `nightly-wasm-atomics` feature** — its ring writes emit genuine
//! `memory.atomic.notify` on the generation word — and asserts the full
//! fast path end-to-end:
//!
//! 1. The stream regions are detected as fast-path capable at attach
//!    (engine registry support + module declares shared memory and
//!    contains atomic notify opcodes).
//! 2. Guest writes wake the host outbound drainer through the unified
//!    region waiter registry directly — echoed data drains promptly, well
//!    under the 1 s backstop.
//! 3. **Zero transition kicks**: `kick_network_waiters` fires on every
//!    guest→host transition (hostcalls, reactor stalls), but every
//!    suppression-eligible region is fast-path, so `kick_count` stays at
//!    zero for the entire run.
//!
//! `#[ignore]`d by default because it requires the atomics guest, which
//! needs a nightly toolchain with `-Zbuild-std` and shared-memory link
//! flags. `scripts/build-all.sh` builds it and keeps the atomics module at
//! `selium_net_demo_atomics.wasm` (the plain build keeps the
//! `selium_net_demo.wasm` path); the underlying command is:
//!
//! ```sh
//! RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals \
//!   -C link-arg=--shared-memory -C link-arg=--max-memory=1073741824" \
//! cargo +nightly build -Zbuild-std=std,panic_abort \
//!   --target wasm32-unknown-unknown -p selium-net-demo \
//!   --features selium-guest/nightly-wasm-atomics
//! cargo test -p selium-runtime --test fastpath_wake -- --ignored
//! ```
//!
//! A stable-built (non-atomics) guest fails this test by design: its
//! regions are not fast-path capable, so transition kicks fire and
//! `kick_count` becomes non-zero.

use std::{
    io::{Read, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

use selium_abi::{Capability, CapabilityGrant, ResourceClass, ResourceSelector};
use selium_runtime::{ReadinessCondition, Runtime, SystemGuestDescriptor};
use selium_service::FlatMsg;

/// Drains the guest's log channel and decodes each frame as a `LogRecord`.
fn drain_logs(runtime: &Runtime, process_id: u64) -> Vec<String> {
    runtime
        .kernel()
        .processes()
        .drain_log_channel(process_id)
        .expect("drain log channel")
        .iter()
        .map(|frame| {
            selium_service::log::LogRecord::decode(frame)
                .expect("decode log record")
                .message
        })
        .collect()
}

/// Task 5.1: a fast-path guest's atomic notify wakes the host drainer with
/// **no runtime kick** for the entire connection lifecycle.
#[test]
#[ignore = "requires the net-demo guest built for wasm32-unknown-unknown with the nightly-wasm-atomics feature (see module docs)"]
fn fastpath_guest_wakes_drainer_with_zero_kicks() {
    let runtime = Runtime::default();
    assert_eq!(
        runtime.kick_count(),
        0,
        "fresh runtime must start with zero kicks"
    );

    let bootstrapped = runtime
        .spawn_system_guest(net_demo_descriptor(net_demo_wasm()))
        .expect("bootstrap atomics net demo guest");
    let process_id = bootstrapped.process_id;

    // Wait until the guest has bound its listener, then discover the bound
    // port through the kernel's listener registry.
    wait_for_log(
        &runtime,
        process_id,
        "net-demo: bound",
        Duration::from_secs(10),
    );
    let addrs = runtime.kernel().network().tcp_listener_addrs();
    assert!(!addrs.is_empty(), "no listener address registered");

    // Connect to the guest's listener: the poller accepts, creates the
    // stream region, and the guest attaches to it — where fast-path
    // detection runs.
    let mut client = std::net::TcpStream::connect(addrs[0]).expect("connect to guest");
    wait_for_log(
        &runtime,
        process_id,
        "net-demo: accepted",
        Duration::from_secs(10),
    );

    // The accepted stream region must become fast-path active: every
    // attacher (the guest) is capable. This is the detection assertion — a
    // stable guest or an engine without the registry fails here. Poll
    // because the guest's AttachRegion completes asynchronously after the
    // "accepted" log.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let regions = runtime.network_wait_regions();
        if !regions.is_empty()
            && regions
                .iter()
                .all(|id| runtime.fast_path_region_active(*id))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "stream regions must be detected as fast-path; a stable-built \
             guest or an engine without the wait registry fails here"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let regions = runtime.network_wait_regions();
    assert!(!regions.is_empty(), "no network proxy regions registered");

    // Snapshot per-region kick counts AFTER detection. A region is
    // kickable between its creation and the guest's attach (the drainer
    // starts before detection); those startup kicks are benign — the ring
    // is empty. Everything from here on must be carried by the fast path.
    let kick_baseline: Vec<(u64, u64)> = regions
        .iter()
        .map(|id| (*id, runtime.region_kick_count(*id)))
        .collect();

    // Write request bytes while the guest task is parked on its inbound
    // ring. The guest wakes via the mailbox, echoes into its outbound ring,
    // and its atomic notify on the generation word must wake the host
    // drainer directly — no kick, no backstop.
    let request = b"ping fastpath";
    let t_write = Instant::now();
    client.write_all(request).expect("write request");
    client.flush().expect("flush request");

    wait_for_log(
        &runtime,
        process_id,
        "net-demo: read done",
        Duration::from_secs(5),
    );
    let mut echo = vec![0_u8; request.len()];
    client.read_exact(&mut echo).expect("read echo");
    assert_eq!(echo, request.to_vec(), "expected the echoed payload");
    assert!(
        t_write.elapsed() < Duration::from_millis(900),
        "echo must drain via the guest's atomic notify well under the 1 s \
         backstop, took {:?}",
        t_write.elapsed()
    );

    // The core assertion: guest→host transitions happened (hostcalls,
    // reactor stalls) but every wake was carried by the fast path — no
    // kicks beyond the startup baseline. Any new kick here means
    // suppression failed, or the guest was built without the atomics
    // feature.
    for (shared_id, baseline) in &kick_baseline {
        assert_eq!(
            runtime.region_kick_count(*shared_id),
            *baseline,
            "transition kicks must be fully suppressed for fast-path region \
             {shared_id} after detection; new kicks mean the guest was likely \
             built without the atomics feature or suppression failed"
        );
    }

    // EOF propagates through the poller into the guest's parked second
    // read, still kick-free.
    drop(client);
    wait_for_log(
        &runtime,
        process_id,
        "net-demo: second read done",
        Duration::from_secs(5),
    );
    for (shared_id, baseline) in &kick_baseline {
        assert_eq!(
            runtime.region_kick_count(*shared_id),
            *baseline,
            "EOF drain must also stay kick-free for fast-path region {shared_id}"
        );
    }

    // No error markers anywhere in the guest run.
    let logs = drain_logs(&runtime, process_id);
    for bad in ["failed", "unexpected EOF"] {
        assert!(
            !logs.iter().any(|message| message.contains(bad)),
            "guest logged {bad:?}: {logs:?}"
        );
    }

    runtime.stop_process(process_id).expect("stop process");
}

fn net_demo_descriptor(module_bytes: Vec<u8>) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "net-demo-atomics".to_string(),
        module_id: "net-demo-atomics-module".to_string(),
        module_bytes,
        entrypoint: "net_demo".to_string(),
        arguments: Vec::new(),
        grants: vec![
            // Selectors within one grant are ANDed, so listener and stream
            // access need separate grants.
            CapabilityGrant::new(
                Capability::Network,
                vec![ResourceSelector::ResourceClass(ResourceClass::TcpListener)],
            ),
            CapabilityGrant::new(
                Capability::Network,
                vec![ResourceSelector::ResourceClass(ResourceClass::TcpStream)],
            ),
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            // Receiving from the bind-created host queue needs HostQueue.
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
        ],
        dependencies: Vec::new(),
        // The test polls the guest log channel itself for phase markers.
        readiness: ReadinessCondition::Immediate,
        tenant: None,
        serving_role: None,
        handlers: Vec::new(),
    }
}

/// Loads the atomics net-demo WASM module and verifies it was built with the
/// `nightly-wasm-atomics` feature (shared memory declaration + atomic notify
/// opcodes). A stale or stable-built artifact produces a precise, actionable
/// panic pointing at the required build command — instead of letting the
/// region-detection assertion further down fail with an ambiguous message.
#[expect(
    clippy::panic,
    reason = "missing/non-atomics build artifact is a hard test failure"
)]
fn net_demo_wasm() -> Vec<u8> {
    let path = net_demo_wasm_path();
    let bytes = std::fs::read(&path).unwrap_or_else(|_error| {
        panic!(
            "atomics net demo guest not found at {}.\n\
                 Build it first with `scripts/build-all.sh` (needs a nightly \
                 toolchain with the rust-src component for -Zbuild-std)",
            path.display()
        )
    });

    let (shared_memory, atomic_notify) = probe_atomics(&bytes);
    assert!(
        shared_memory && atomic_notify,
        "atomics net demo guest at {} was NOT built with the \
         nightly-wasm-atomics feature (shared memory = {shared_memory}, \
         atomic notify = {atomic_notify}).\n\
         This test requires an atomics-capable guest and fails by design on a \
         stable (non-atomics) build.\n\
         Rebuild it with `scripts/build-all.sh`.",
        path.display()
    );
    bytes
}

/// Returns the path to the compiled atomics net-demo WASM module.
///
/// The atomics build is kept at a distinct file name (`..._atomics.wasm`) by
/// `scripts/build-all.sh`, because `cargo` emits `selium_net_demo.wasm` for a
/// crate named `selium-net-demo` and the plain build must keep that path.
fn net_demo_wasm_path() -> PathBuf {
    let target_dir =
        std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_e| "../../target".to_string());
    PathBuf::from(target_dir).join("wasm32-unknown-unknown/debug/selium_net_demo_atomics.wasm")
}

/// Minimal WASM scan for the fast-path signals: any memory entry declaring a
/// shared flag (bit 1 of the limits flags), and the `memory.atomic.notify`
/// opcode sequence (`0xFE 0x00`) in the code section. Malformed modules
/// report `false` (the safe fallback).
fn probe_atomics(module: &[u8]) -> (bool, bool) {
    let mut shared_memory = false;
    let mut atomic_notify = false;
    if module.len() < 8
        || module.get(..4) != Some(b"\0asm")
        || module.get(4..8) != Some(&[0x01, 0x00, 0x00, 0x00])
    {
        return (false, false);
    }
    let mut pos = 8usize;
    while pos < module.len() {
        let Some((id, payload)) = read_section(module, &mut pos) else {
            break;
        };
        match id {
            5 => shared_memory |= scan_shared_memory(payload),
            10 => atomic_notify |= payload.windows(2).any(|pair| pair == [0xFE, 0x00]),
            _ => {}
        }
    }
    (shared_memory, atomic_notify)
}

fn read_leb_u32(bytes: &[u8], mut pos: usize) -> Option<(u32, usize)> {
    let mut result: u32 = 0;
    let mut shift = 0;
    loop {
        let byte = *bytes.get(pos)?;
        pos += 1;
        result |= u32::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Some((result, pos));
        }
        shift += 7;
        if shift >= 32 {
            return None;
        }
    }
}

fn read_section<'a>(bytes: &'a [u8], pos: &mut usize) -> Option<(u8, &'a [u8])> {
    let id = *bytes.get(*pos)?;
    *pos += 1;
    let (size, next) = read_leb_u32(bytes, *pos)?;
    let payload = bytes.get(next..next + size as usize)?;
    *pos = next + size as usize;
    Some((id, payload))
}

fn scan_shared_memory(payload: &[u8]) -> bool {
    let (count, mut pos) = match read_leb_u32(payload, 0) {
        Some(v) => v,
        None => return false,
    };
    for _ in 0..count {
        let Some(flags) = payload.get(pos).copied() else {
            return false;
        };
        pos += 1;
        let (_, next) = match read_leb_u32(payload, pos) {
            Some(v) => v,
            None => return false,
        };
        pos = next;
        if flags & 0x01 != 0 {
            let Some((_, next)) = read_leb_u32(payload, pos) else {
                return false;
            };
            pos = next;
        }
        if flags & 0x02 != 0 {
            return true;
        }
    }
    false
}

/// Polls the guest log channel until a message containing `needle` appears.
#[expect(clippy::panic, reason = "test helper")]
fn wait_for_log(runtime: &Runtime, process_id: u64, needle: &str, timeout: Duration) {
    let mut seen: Vec<String> = Vec::new();
    let start = Instant::now();
    while start.elapsed() < timeout {
        let fresh = drain_logs(runtime, process_id);
        seen.extend(fresh);
        if seen.iter().any(|message| message.contains(needle)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {needle:?} in guest log; got {seen:?}");
}
