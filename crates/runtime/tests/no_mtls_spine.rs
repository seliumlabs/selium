//! No-mTLS spine integration test (task 5.1): the connector's mTLS opt-in
//! path with no identity guest deployed.
//!
//! The connector serves without client authentication and stream handoffs
//! carry empty identity metadata — the deployment shape for user guests
//! deliberately serving public, unauthorised endpoints. The
//! identity-requiring bridge refuses the unauthenticated handoff: it can
//! confer grants for no one.
//!
//! `#[ignore]`d by default because it requires the guests built for
//! `wasm32-unknown-unknown` first (see the identity spine test's build
//! instructions; this test needs only `selium-discovery`,
//! `selium-connector-quic` and `selium-bridge`).
//!
//! The connector binds a fixed listener (`0.0.0.0:4433`); this test and the
//! golden-path spine test live in separate binaries (one runtime per
//! process — the runtime installs a process-global region provider) and
//! serialise on the cross-process spine port lock.

use std::time::Duration;

use selium_runtime::{Runtime, RuntimeConfig};
use selium_service::{ControlRequest, ControlResponse};
use spine_common::{
    CONNECTOR_ADDR, CONTROL_URI, SpinePortGuard, bridge_server_descriptor,
    client_options_no_identity, connector_descriptor, discovery_descriptor, drain_logs, read_wasm,
    seed_tls_blob_store, wait_for_logs,
};

mod common;
mod spine_common;

/// mTLS opt-in (task 5.1): with no identity guest deployed, the connector
/// serves without client authentication and handoffs carry empty identity
/// metadata — the deployment shape for user guests deliberately serving
/// public, unauthorised endpoints. The identity-requiring bridge refuses the
/// unauthenticated handoff: it can confer grants for no one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the discovery, connector and bridge guests built for wasm32-unknown-unknown"]
async fn no_mtls_connector_serves_but_bridge_refuses_unauthenticated_handoffs() {
    // The connector binds a fixed listener; hold the port guard for the
    // whole test so the golden-path test cannot race it.
    let _port = SpinePortGuard::acquire();

    let runtime = Runtime::default();
    seed_tls_blob_store(&runtime);

    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            domain_table: Vec::new(),
            // No identity guest: mTLS is off (opt-in via identity deployment).
            system_guests: vec![
                discovery_descriptor(read_wasm("selium-discovery", "selium_discovery.wasm")),
                connector_descriptor(
                    read_wasm("selium-connector-quic", "selium_connector_quic.wasm"),
                    vec!["discovery".to_string()],
                ),
                bridge_server_descriptor(
                    read_wasm("selium-bridge", "selium_bridge.wasm"),
                    vec!["discovery".to_string(), "quic-connector".to_string()],
                ),
            ],
        })
        .unwrap_or_else(|error| {
            // The improved bootstrap diagnostics carry the failed guest's
            // own logs in the activity message.
            let activity: Vec<String> = runtime
                .activity_log()
                .into_iter()
                .map(|event| format!("activity: {:?}", event.message))
                .collect();
            panic!("bootstrap no-mTLS spine: {error}; state: {activity:#?}")
        });

    let find = |name: &str| {
        report
            .guests
            .iter()
            .find(|guest| guest.name == name)
            .unwrap_or_else(|| panic!("bootstrap report contains guest {name}"))
            .process_id
    };
    let connector = find("quic-connector");
    let bridge = find("bridge-server");

    // The connector reports the mTLS opt-out (not a fail-closed refusal).
    let _ = wait_for_logs(
        &runtime,
        connector,
        &[
            ("quic-connector: listening on", 1),
            ("serving without client authentication", 1),
        ],
        Duration::from_secs(30),
    );

    // A certless client completes the QUIC handshake against the wasm
    // connector: the public-endpoint deployment shape.
    let connecting = selium_client::connect(
        CONNECTOR_ADDR.parse().expect("connector address"),
        client_options_no_identity(),
    );
    let client = match tokio::time::timeout(Duration::from_secs(240), connecting).await {
        Ok(result) => result.expect("certless client connection"),
        Err(_elapsed) => panic!("certless handshake timed out"),
    };

    // Opening a route fails: the handoff carries no identity metadata, so
    // the bridge refuses it (attach-then-close) and the connector FINs the
    // client stream — the serving side never accepts the handshake.
    let opening = client.rpc::<ControlRequest, ControlResponse>(CONTROL_URI);
    match tokio::time::timeout(Duration::from_secs(120), opening).await {
        Ok(Err(_refused)) => {}
        Ok(Ok(_rpc)) => panic!("the bridge must refuse an unauthenticated handoff"),
        Err(_elapsed) => panic!("unauthenticated open did not settle"),
    }

    // The bridge logged the refusal of the unauthenticated handoff.
    let _ = wait_for_logs(
        &runtime,
        bridge,
        &[("refusing handoff with unparseable identity", 1)],
        Duration::from_secs(30),
    );

    for (process_id, name) in [(connector, "quic-connector"), (bridge, "bridge-server")] {
        let logs = drain_logs(&runtime, process_id);
        assert!(
            !logs.iter().any(|message| message.contains("failed")),
            "{name} guest logged an error: {logs:?}"
        );
    }

    drop(client);
    runtime.stop_process(bridge).expect("stop bridge-server");
    runtime.stop_process(connector).expect("stop connector");
}
