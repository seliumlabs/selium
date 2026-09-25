//! Identity golden-path integration test (task 7.2).
//!
//! Drives the single-host spine end to end: the identity guest mints the
//! `acme` tenant CA (operator tier), issues a user leaf certificate from a
//! test-generated client SPKI, and records that principal's baseline grants
//! (tenant tier). The connector then trusts the *identity-published* anchor
//! (no static `tls-certs` client-anchor manifests), a native `selium-client`
//! completes mTLS with the issued leaf, and the bridge-server resolves the
//! authenticated fingerprint against the identity-published grant table to
//! spawn a bridge-channel — which rendezvouses the control plane's served
//! route and completes a typed `deploy`/`status` round trip.
//!
//! The revocation leg (task 4.6) then exercises the tenant-CA revocation
//! semantics end to end: a second invocation of the onboarding guest revokes
//! the `acme` tenant, the connector rebuilds its union verifier without the
//! anchor, the already-established connection keeps working until
//! re-authentication, and the next connection presenting the revoked
//! tenant's leaf cannot carry a stream.
//!
//! The operator tier is exercised by a dedicated onboarding guest
//! (`selium-identity-onboard`), which receives the test-generated client SPKI
//! as an entrypoint pointer argument, an integer entry mode (`0` = onboard,
//! `1` = revoke), and writes the issued leaf to a blob store the host reads
//! back for the mTLS client.
//!
//! `#[ignore]`d by default because it requires the guests built for
//! `wasm32-unknown-unknown` first (release profile — the TLS handshake runs
//! through the wasm interpreter, too slow at debug optimization for the
//! quinn timeouts):
//!
//! ```sh
//! cargo build --release --target wasm32-unknown-unknown \
//!   -p selium-discovery -p selium-identity -p selium-identity-onboard \
//!   -p selium-connector-quic -p selium-bridge -p selium-bridge-channel \
//!   -p selium-control-plane
//! cargo test -p selium-runtime --test identity_spine -- --ignored
//! ```
//!
//! The connector binds a fixed listener (`0.0.0.0:4433`, see its
//! `QUIC_LISTEN_ADDR`); this test and the no-mTLS spine test live in
//! separate binaries (one runtime per process — the runtime installs a
//! process-global region provider) and serialise on the cross-process
//! spine port lock.

use std::{sync::Arc, time::Duration};

use rcgen::{KeyPair, PKCS_ECDSA_P256_SHA256, PublicKeyData};
use selium_client::FlatMsg as _;
use selium_identity_onboard::{MODE_ONBOARD, MODE_REVOKE};
use selium_runtime::{Runtime, RuntimeConfig};
use selium_service::{ControlRequest, ControlResponse, DelegationStatus, Deployment};
use spine_common::{
    CONNECTOR_ADDR, CONTROL_URI, SCHEDULER_DEFERRED, SpinePortGuard, bridge_server_descriptor,
    client_options, connector_descriptor, control_plane_descriptor, discovery_descriptor,
    drain_logs, identity_descriptor, operator_descriptor, read_issued_leaf, read_wasm,
    seed_tls_blob_store, wait_for_logs,
};

mod common;
mod spine_common;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the identity, discovery, connector, bridge and control-plane guests built for wasm32-unknown-unknown"]
async fn identity_spine_onboards_and_reaches_the_control_plane() {
    // The connector binds a fixed listener; hold the port guard for the
    // whole test so the no-mTLS test cannot race it.
    let _port = SpinePortGuard::acquire();

    let runtime = Runtime::default();
    runtime.generate_keyring().expect("generate host keyring");
    seed_tls_blob_store(&runtime);

    // The mTLS client's key pair, generated host-side; only its SPKI crosses
    // into the identity guest, and identity returns the signed leaf.
    let client_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("client key");
    let client_spki = client_key.subject_public_key_info();
    let client_pkcs8 = client_key.serialize_der();

    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            domain_table: Vec::new(),
            system_guests: vec![
                discovery_descriptor(read_wasm("selium-discovery", "selium_discovery.wasm")),
                identity_descriptor(read_wasm("selium-identity", "selium_identity.wasm")),
                operator_descriptor(
                    read_wasm("selium-identity-onboard", "selium_identity_onboard.wasm"),
                    client_spki.clone(),
                    MODE_ONBOARD,
                    vec!["discovery".to_string(), "identity".to_string()],
                ),
                connector_descriptor(
                    read_wasm("selium-connector-quic", "selium_connector_quic.wasm"),
                    vec!["discovery".to_string(), "identity".to_string()],
                ),
                bridge_server_descriptor(
                    read_wasm("selium-bridge", "selium_bridge.wasm"),
                    vec![
                        "discovery".to_string(),
                        "identity".to_string(),
                        "quic-connector".to_string(),
                    ],
                ),
                control_plane_descriptor(read_wasm(
                    "selium-control-plane",
                    "selium_control_plane.wasm",
                )),
            ],
        })
        .unwrap_or_else(|error| {
            let mut logs = vec![format!("activity: {:?}", error.to_string())];
            for event in runtime.activity_log() {
                logs.push(format!("activity: {}", event.message));
                if let Some(process_id) = event.process_id
                    && let Ok(messages) = runtime.kernel().processes().drain_log_channel(process_id)
                {
                    logs.extend(messages.into_iter().filter_map(|frame| {
                        selium_service::log::LogRecord::decode(&frame)
                            .ok()
                            .map(|record| format!("pid {process_id}: {}", record.message))
                    }));
                }
            }
            panic!("bootstrap identity spine: {error}; state: {logs:#?}")
        });

    let find = |name: &str| {
        report
            .guests
            .iter()
            .find(|guest| guest.name == name)
            .unwrap_or_else(|| panic!("bootstrap report contains guest {name}"))
            .process_id
    };
    let identity = find("identity");
    let connector = find("quic-connector");
    let bridge = find("bridge-server");
    let control_plane = find("control-plane");

    runtime
        .register_module_bytes(
            "bridge-channel-module".to_string(),
            read_wasm("selium-bridge-channel", "selium_bridge_channel.wasm"),
        )
        .expect("register bridge-channel module");

    // Identity and the onboarding operator reported ready (bootstrap gates on
    // it), so the tenant CA is minted, the leaf issued, and grants recorded.
    let _ = identity;

    // Wait for the connector to consume the identity-published anchor.
    let _ = wait_for_logs(
        &runtime,
        connector,
        &[
            ("quic-connector: listening on", 1),
            ("rebuilt client anchor set", 1),
        ],
        Duration::from_secs(30),
    );

    // Diagnostic collector mirrors the spine test's failure-path handling.
    let collected: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let stop_collector = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let collector = {
        let runtime = runtime.clone();
        let collected = collected.clone();
        let stop = stop_collector.clone();
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(250));
                for process_id in [connector, bridge, control_plane] {
                    for message in drain_logs(&runtime, process_id) {
                        collected
                            .lock()
                            .expect("collector lock")
                            .push(format!("pid {process_id}: {message}"));
                    }
                }
            }
        })
    };
    let logs = || {
        let mut snapshot = collected.lock().expect("collector lock").clone();
        for event in runtime.activity_log() {
            if let Some(process_id) = event.process_id
                && let Ok(messages) = runtime.kernel().processes().drain_log_channel(process_id)
            {
                snapshot.extend(messages.into_iter().filter_map(|frame| {
                    selium_service::log::LogRecord::decode(&frame)
                        .ok()
                        .map(|record| format!("pid {process_id}: {}", record.message))
                }));
            }
        }
        snapshot.sort();
        snapshot
    };
    let dump = || {
        let mut out = logs();
        out.extend(
            runtime
                .activity_log()
                .into_iter()
                .map(|event| format!("activity: {:?}", event.message)),
        );
        out
    };

    // mTLS handshake with the identity-issued leaf against the wasm connector.
    let leaf = read_issued_leaf(&runtime);
    let connecting = selium_client::connect(
        CONNECTOR_ADDR.parse().expect("connector address"),
        client_options(leaf.clone(), client_pkcs8.clone()),
    );
    let client = match tokio::time::timeout(Duration::from_secs(240), connecting).await {
        Ok(result) => result.expect("client connection"),
        Err(_elapsed) => panic!("handshake timed out; state: {:#?}", dump()),
    };

    let opening = client.rpc::<ControlRequest, ControlResponse>(CONTROL_URI);
    let mut rpc = match tokio::time::timeout(Duration::from_secs(120), opening).await {
        Ok(result) => result.expect("control route channel open"),
        Err(_elapsed) => panic!("channel open timed out; state: {:#?}", dump()),
    };

    let deploying = rpc.request(ControlRequest::Deploy {
        workload_id: "api".to_string(),
        replicas: 3,
        module: "api/v1".to_string(),
    });
    let accepted = match tokio::time::timeout(Duration::from_secs(120), deploying).await {
        Ok(result) => result.expect("deploy round trip"),
        Err(_elapsed) => panic!("deploy timed out; guest logs: {:#?}", logs()),
    };
    assert_eq!(
        accepted,
        ControlResponse::Accepted {
            workload_id: "api".to_string(),
            replicas: 3,
            module: "api/v1".to_string(),
            delegated: DelegationStatus {
                step: "scheduler".to_string(),
                applied: false,
                context: SCHEDULER_DEFERRED.to_string(),
            },
        },
        "deploy accepted with the typed deferred scheduler status; guest logs: {:#?}",
        logs(),
    );

    let status = rpc
        .request(ControlRequest::Status {
            workload_id: "api".to_string(),
        })
        .await
        .expect("status round trip");
    assert_eq!(
        status,
        ControlResponse::Status {
            deployment: Some(Deployment {
                workload_id: "api".to_string(),
                replicas: 3,
                module: "api/v1".to_string(),
            }),
        },
        "status returns the recorded desired state; guest logs: {:#?}",
        logs(),
    );

    // Revocation leg (task 4.6): the operator tier revokes the `acme`
    // tenant, removing its published anchor and deleting its CA key.
    // Stop the diagnostic collector first so the log waits below see
    // every message (the collector drains concurrently).
    stop_collector.store(true, std::sync::atomic::Ordering::Relaxed);
    collector.join().expect("collector thread");

    // `spawn_system_guest` does not perform `bootstrap_system_guests`'s
    // discovery-handle wiring (the leading `Context` slot and the
    // explicit-resource grant), so the driver is booted through a second
    // bootstrap call against the already-running discovery.
    let revoke_guest = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: false,
            domain_table: Vec::new(),
            system_guests: vec![operator_descriptor(
                read_wasm("selium-identity-onboard", "selium_identity_onboard.wasm"),
                Vec::new(),
                MODE_REVOKE,
                // A second `bootstrap_system_guests` call validates
                // dependencies only against its own guest list, so the
                // already-running discovery/identity are not listed.
                Vec::new(),
            )],
        })
        .map(|report| {
            report
                .guests
                .first()
                .cloned()
                .expect("revocation driver booted")
        })
        .unwrap_or_else(|error| {
            panic!("bootstrap revocation driver: {error}; state: {:#?}", dump())
        });
    assert_eq!(revoke_guest.name, "identity-onboard");

    // The connector rebuilds its union verifier without the revoked anchor.
    let rebuild_logs = wait_for_logs(
        &runtime,
        connector,
        &[("rebuilt client anchor set", 1)],
        Duration::from_secs(60),
    );
    let rebuilds: Vec<String> = rebuild_logs
        .iter()
        .filter(|message| message.contains("rebuilt client anchor set"))
        .cloned()
        .collect();
    // The post-revocation rebuild must carry zero live anchors: the
    // tombstone drops the revoked tenant out of the rebuilt union verifier.
    assert!(
        rebuilds
            .iter()
            .any(|message| message.contains("(0 anchors)")),
        "the post-revocation rebuild must list zero anchors: {rebuilds:?}"
    );

    // Live connections survive revocation until re-authentication: the
    // already-established client connection still serves requests.
    let surviving = rpc
        .request(ControlRequest::Status {
            workload_id: "api".to_string(),
        })
        .await
        .expect("live connection survives tenant revocation");
    assert_eq!(
        surviving,
        ControlResponse::Status {
            deployment: Some(Deployment {
                workload_id: "api".to_string(),
                replicas: 3,
                module: "api/v1".to_string(),
            }),
        },
        "the established connection stays valid until re-authentication; guest logs: {:#?}",
        logs(),
    );

    // The next connection presenting the revoked tenant's leaf is refused:
    // the TLS 1.3 client completes its side of the handshake on the server's
    // first flight — before the server verifies the client certificate — so
    // the refusal surfaces when the connection is used (the server's alert
    // or refusal closes it), mirroring the mTLS-off test's refusal shape.
    let refused = selium_client::connect(
        CONNECTOR_ADDR.parse().expect("connector address"),
        client_options(leaf, client_pkcs8),
    );
    let refused_client = match tokio::time::timeout(Duration::from_secs(240), refused).await {
        Ok(Ok(client)) => client,
        // The handshake itself was refused: even better.
        Ok(Err(_refused)) => {
            drop(rpc);
            drop(client);
            return;
        }
        Err(_elapsed) => panic!("revoked connection did not settle; state: {:#?}", dump()),
    };
    let opening = refused_client.rpc::<ControlRequest, ControlResponse>(CONTROL_URI);
    match tokio::time::timeout(Duration::from_secs(120), opening).await {
        Ok(Err(_refused)) => {}
        Ok(Ok(_rpc)) => panic!(
            "the revoked tenant's leaf must not carry a stream; post-revocation rebuilds: {rebuilds:?}; state: {:#?}",
            dump()
        ),
        Err(_elapsed) => panic!("revoked stream did not settle; state: {:#?}", dump()),
    }

    for (process_id, name) in [
        (identity, "identity"),
        (connector, "quic-connector"),
        (bridge, "bridge-server"),
        (control_plane, "control-plane"),
        (revoke_guest.process_id, "identity-onboard"),
    ] {
        let logs = drain_logs(&runtime, process_id);
        // The refused post-revocation handshake is expected to log
        // incoming-connection and client-auth failures on the connector
        // ("no client trust anchors configured" — the empty rebuilt union
        // verifier refusing the revoked tenant's certificate); anything
        // else is a real error.
        let unexpected: Vec<&String> = logs
            .iter()
            .filter(|message| {
                message.contains("failed")
                    && !(name == "quic-connector"
                        && (message.contains("incoming connection failed")
                            || message.contains("no client trust anchors configured")))
            })
            .collect();
        assert!(
            unexpected.is_empty(),
            "{name} guest logged an error: {logs:?}"
        );
    }

    drop(rpc);
    drop(client);
    runtime
        .stop_process(control_plane)
        .expect("stop control-plane");
    runtime.stop_process(bridge).expect("stop bridge-server");
    runtime.stop_process(connector).expect("stop connector");
    runtime.stop_process(identity).expect("stop identity");
    runtime
        .stop_process(revoke_guest.process_id)
        .expect("stop revocation driver");
}
