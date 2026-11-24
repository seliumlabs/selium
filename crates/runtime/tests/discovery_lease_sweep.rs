//! Discovery's lease sweep ticks inside a real `wasm32` guest
//! (`cross-guest-wake-delivery`, task 4.3).
//!
//! The sweep is a `Timer` loop in the discovery guest. A timer that never fires
//! is indistinguishable from one that fires after the heat death of the
//! universe: nothing else in the platform depends on the sweep, so nothing else
//! would notice. This pins the tick.
//!
//! It lives in its own test binary because a process may hold only one
//! `Runtime` — the region provider is process-global — so each scenario needs
//! its own binary.
//!
//! `#[ignore]`d by default because it requires the discovery guest built for
//! `wasm32-unknown-unknown`:
//!
//! ```sh
//! cargo build --target wasm32-unknown-unknown -p selium-discovery
//! cargo test -p selium-runtime --test discovery_lease_sweep -- --ignored
//! ```

use std::time::{Duration, Instant};

use selium_abi::{Capability, CapabilityGrant, ResourceClass, ResourceSelector};
use selium_runtime::{Runtime, RuntimeConfig, SystemGuestDescriptor};
use selium_service::FlatMsg;

mod common;

/// Bound on the first tick. The sweep interval is 2 s; anything slower is a
/// sweep parked on a deadline that never arrives, which is the failure this
/// test exists to catch.
const FIRST_TICK_BOUND: Duration = Duration::from_secs(30);

fn discovery_descriptor(module_bytes: Vec<u8>) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "discovery".to_string(),
        module_id: "discovery-module".to_string(),
        module_bytes,
        entrypoint: "discovery_main".to_string(),
        arguments: Vec::new(), // populated by bootstrap
        grants: vec![
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
            CapabilityGrant::new(Capability::ProcessProbe, Vec::new()),
            CapabilityGrant::new(
                Capability::RouteClaim,
                vec![ResourceSelector::Uri("sel:///discovery-routes".to_string())],
            ),
            CapabilityGrant::new(
                Capability::RouteClaim,
                vec![ResourceSelector::Uri("sel:///discovery".to_string())],
            ),
        ],
        tenant: None,
        discovery_service: true,
        handlers: Vec::new(),
    }
}

// scenario: route-watch/registration-leases/lease-expiry-fires-on-the-timer-schedule
// scenario: discovery-registration/lease-sweep-on-the-monotonic-timer/sweep-ticks-in-a-wasm32-guest
// scenario: discovery-registration/lease-sweep-on-the-monotonic-timer/a-dead-sweep-is-observable
#[test]
#[ignore = "requires the discovery guest built for wasm32-unknown-unknown"]
fn discovery_lease_sweep_ticks_on_its_timer() {
    let runtime = Runtime::default();
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: vec![discovery_descriptor(common::read_guest_wasm_debug(
                "selium-discovery",
                "selium_discovery.wasm",
            ))],
            state_dir: None,
        })
        .expect("bootstrap discovery");
    let discovery_id = report
        .guests
        .iter()
        .find(|guest| guest.name == "discovery")
        .expect("discovery in report")
        .process_id;

    let started = Instant::now();
    wait_for_line(
        &runtime,
        discovery_id,
        "discovery lease sweep tick",
        FIRST_TICK_BOUND,
    );
    let first_tick = started.elapsed();
    assert!(
        first_tick < FIRST_TICK_BOUND,
        "the sweep must tick on its timer; first tick at {first_tick:?}"
    );
}

/// Polls the guest log channel until a line starting with `prefix` appears.
#[expect(clippy::panic, reason = "test helper")]
fn wait_for_line(runtime: &Runtime, process_id: u64, prefix: &str, timeout: Duration) -> String {
    let mut seen: Vec<String> = Vec::new();
    let start = Instant::now();
    while start.elapsed() < timeout {
        let frames = runtime
            .kernel()
            .processes()
            .drain_log_channel(process_id)
            .expect("drain log channel");
        seen.extend(frames.iter().map(|frame| {
            selium_service::log::LogRecord::decode(frame)
                .expect("decode log record")
                .message
        }));
        if let Some(found) = seen.iter().find(|message| message.starts_with(prefix)) {
            return found.clone();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {prefix:?} in guest log; got {seen:?}");
}
