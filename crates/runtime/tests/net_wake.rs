//! Event-driven network proxy wake integration tests.
//!
//! Deploys the real `selium-net-demo` WASM guest and verifies the two
//! wake paths of `event-driven-net-proxies` end-to-end:
//!
//! 1. **Task 4.1** — a guest task parked on an inbound network ring
//!    (via `WaitRegister`) wakes on socket data through the mailbox,
//!    with no sleep-based polling.
//! 2. **Task 4.2** — a guest write followed by a reactor stall is drained
//!    to the socket by the stall kick, well under the bounded backstop.
//!
//! `#[ignore]`d by default because it requires the demo guest built for
//! `wasm32-unknown-unknown` first:
//!
//! ```sh
//! cargo build --target wasm32-unknown-unknown -p selium-net-demo
//! cargo test -p selium-runtime --test net_wake -- --ignored
//! ```

use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};

use selium_abi::{Capability, CapabilityGrant, ResourceClass, ResourceSelector};
use selium_runtime::{ReadinessCondition, Runtime, SystemGuestDescriptor};
use selium_service::FlatMsg;

mod common;

fn all_logs(runtime: &Runtime, process_id: u64) -> Vec<String> {
    drain_logs(runtime, process_id)
}

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

/// Task 4.1 + 4.2: guest reader parked on an inbound ring wakes on socket
/// data (WaitRegister → mailbox), echoes, and its post-write stall drains
/// to the socket without waiting for the backstop timeout.
#[test]
#[ignore = "requires the net-demo guest built for wasm32-unknown-unknown"]
fn guest_park_wakes_on_socket_data_and_stall_drains_promptly() {
    let runtime = Runtime::default();
    let bootstrapped = runtime
        .spawn_system_guest(net_demo_descriptor(net_demo_wasm()))
        .expect("bootstrap net demo guest");
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

    // Connect to the guest's listener. The kernel poller accepts, creates
    // the stream region, and enqueues it — no accept polling anywhere.
    let mut client = std::net::TcpStream::connect(addrs[0]).expect("connect to guest");

    // Give the guest's accept task a chance to park its read on the inbound
    // ring (issuing WaitRegister) before writing request bytes.
    wait_for_log(
        &runtime,
        process_id,
        "net-demo: accepted",
        Duration::from_secs(10),
    );

    // Write request bytes while the guest task is parked on its inbound
    // ring. The poller thread delivers the wake end-to-end (mailbox +
    // inline reactor poll) — no embedder-side pumping involved.
    let request = b"ping event-driven";
    let t_write = Instant::now();
    client.write_all(request).expect("write request");
    client.flush().expect("flush request");

    wait_for_log(
        &runtime,
        process_id,
        "net-demo: read done",
        Duration::from_secs(5),
    );
    let wake_latency = t_write.elapsed();
    assert!(
        wake_latency < Duration::from_secs(1),
        "guest park must wake via mailbox well under the backstop, took {wake_latency:?}"
    );

    // The guest echoed into its outbound ring and then stalled on a second
    // read. The stall kick must drain the echo promptly; reading it here
    // must not require the 1 s backstop.
    let mut echo = vec![0_u8; request.len()];
    client.read_exact(&mut echo).expect("read echo");
    assert_eq!(echo, request.to_vec(), "expected the echoed payload");
    assert!(
        t_write.elapsed() < Duration::from_secs(1),
        "echo must drain via the stall kick without the backstop, took {:?}",
        t_write.elapsed()
    );

    // Close the client: EOF propagates through the poller into the guest's
    // parked second read, which must wake via the same WaitRegister bridge.
    drop(client);
    wait_for_log(
        &runtime,
        process_id,
        "net-demo: second read done",
        Duration::from_secs(5),
    );

    // No error markers anywhere in the guest run.
    let logs = all_logs(&runtime, process_id);
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
        name: "net-demo".to_string(),
        module_id: "net-demo-module".to_string(),
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

/// Reads the plain net-demo WASM module, rebuilding it if its inputs
/// changed (a no-op when fresh) and failing loudly if the build or read
/// fails.
fn net_demo_wasm() -> Vec<u8> {
    common::read_guest_wasm_debug("selium-net-demo", "selium_net_demo.wasm")
}

/// Polls the guest log channel until a message containing `needle` appears.
#[expect(clippy::panic, reason = "test helper")]
fn wait_for_log(runtime: &Runtime, process_id: u64, needle: &str, timeout: Duration) -> Duration {
    let mut seen: Vec<String> = Vec::new();
    let start = Instant::now();
    while start.elapsed() < timeout {
        let fresh = drain_logs(runtime, process_id);
        seen.extend(fresh);
        if seen.iter().any(|message| message.contains(needle)) {
            return start.elapsed();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {needle:?} in guest log; got {seen:?}");
}
