//! Golden-path spine test.
//!
//! Deploys the real `selium-spine-demo` WASM guest and asserts on the
//! platform spine end-to-end: module load, hostcall ABI, shared-memory
//! region alloc/attach into guest linear memory, log channel drain,
//! typed pub/sub over a shared-memory channel, readiness signalling, and
//! process teardown.
//!
//! This test is `#[ignore]`d by default because it requires the demo guest
//! to be built for `wasm32-unknown-unknown` first:
//!
//! ```sh
//! cargo build --target wasm32-unknown-unknown -p selium-spine-demo
//! cargo test -p selium-runtime --test spine -- --ignored
//! ```

use selium_abi::{Capability, CapabilityGrant, ResourceClass, ResourceSelector};
use selium_runtime::{ReadinessCondition, Runtime, SystemGuestDescriptor};
use selium_service::FlatMsg;

mod common;

/// Drains the guest's log channel and decodes each frame as a `LogRecord`.
fn drain_log_messages(runtime: &Runtime, process_id: u64) -> Vec<String> {
    let frames = runtime
        .kernel()
        .processes()
        .drain_log_channel(process_id)
        .expect("drain log channel");
    frames
        .iter()
        .map(|frame| {
            selium_service::log::LogRecord::decode(frame)
                .expect("decode log record")
                .message
        })
        .collect()
}

fn spine_demo_descriptor(module_bytes: Vec<u8>) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "spine-demo".to_string(),
        module_id: "spine-demo-module".to_string(),
        module_bytes,
        entrypoint: "spine_demo".to_string(),
        arguments: Vec::new(),
        grants: vec![CapabilityGrant::new(
            Capability::SharedMemory,
            vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
        )],
        dependencies: Vec::new(),
        readiness: ReadinessCondition::ActivityLogContains("guest ready".to_string()),
        tenant: None,
        serving_role: None,
        handlers: Vec::new(),
    }
}

/// Reads the spine-demo WASM module, rebuilding it if its inputs changed
/// (a no-op when fresh) and failing loudly if the build or read fails.
fn spine_demo_wasm() -> Vec<u8> {
    common::read_guest_wasm_debug("selium-spine-demo", "selium_spine_demo.wasm")
}

#[test]
#[ignore = "requires the spine-demo guest built for wasm32-unknown-unknown"]
fn wasm_guest_runs_golden_path() {
    let runtime = Runtime::default();
    let bootstrapped = runtime
        .spawn_system_guest(spine_demo_descriptor(spine_demo_wasm()))
        .expect("bootstrap spine demo guest");
    let process_id = bootstrapped.process_id;

    // The guest signalled readiness via `mark_ready()` from inside WASM.
    let activity = runtime.activity_log();
    assert!(
        activity
            .iter()
            .any(|event| event.process_id == Some(process_id)
                && event.message.contains("guest ready")),
        "expected a GuestReady activity event, got: {activity:?}"
    );

    // The guest initialised its log transport inside WASM (AllocRegion +
    // AttachRegion mapping into linear memory) and published structured log
    // records. The kernel drains them as frames from shared memory.
    let messages = drain_log_messages(&runtime, process_id);
    assert!(
        messages.iter().any(|message| message == "hello spine"),
        "expected 'hello spine' in guest log, got: {messages:?}"
    );
    // The guest created a shared-memory channel and completed a typed
    // pub/sub round trip inside WASM.
    assert!(
        messages.iter().any(|message| message == "spine: pubsub ok"),
        "expected 'spine: pubsub ok' in guest log, got: {messages:?}"
    );

    // Metering flows end-to-end from the engine: the host projects the guest's
    // executed-instruction count read from the Wasmtiny instance meter (no
    // host-side placeholder hook).
    runtime.metering_tick();
    let observation = runtime
        .kernel()
        .processes()
        .metering_observation(process_id)
        .expect("metered observation");
    assert!(
        observation.cpu_instructions > 0,
        "the guest's executed instructions must be projected: {observation:?}"
    );

    // Teardown: stop releases runtime state and kernel resources.
    runtime.stop_process(process_id).expect("stop process");
    assert_eq!(runtime.loaded_guest_count(), 0);
    assert!(runtime.stop_process(process_id).is_err());
}
