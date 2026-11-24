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
//! through the wasm interpreter, too slow at debug optimisation for the
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

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Duration,
};

use rcgen::{KeyPair, PKCS_ECDSA_P256_SHA256, PublicKeyData};
use selium_client::FlatMsg as _;
use selium_identity_onboard::{MODE_ONBOARD, MODE_REVOKE};
use selium_runtime::RuntimeConfig;
use selium_service::{ControlRequest, ControlResponse, DelegationStatus, Deployment};
use spine_common::{
    BRIDGE_CERT_PEM, CONNECTOR_ADDR, CONTROL_URI, CONVERGENCE_BOUND, HANDSHAKE_BOUND,
    OBSERVATION_BOUND, SCHEDULER_DEFERRED, SpinePortGuard, StateRoot, bridge_server_descriptor,
    client_options, connector_descriptor, control_plane_descriptor, discovery_descriptor,
    drain_logs_if_live, identity_descriptor, operator_descriptor, read_issued_leaf, read_wasm,
    runtime_in_state_root, seed_tls_blob_store, store_descriptor, wait_for_logs,
};

mod common;
mod spine_common;
// scenario: selium-identity/user-certificate-lifecycle/issuing-a-user-cert-records-the-fingerprint
// scenario: selium-cli/ephemeral-single-request-lifecycle/one-invocation-performs-one-request

// scenario: verification-gate/external-protocol-paths-have-real-end-to-end-evidence/cli-subprocess-uses-real-bridge
// scenario: selium-cli/bridge-derived-connection/real-cli-invocation-reaches-the-control-plane
// scenario: control-plane/served-typed-control-surface/external-client-reaches-the-served-surface
// scenario: control-plane/capability-enforced-access/privileged-client-reaches-the-control-surface
// scenario: control-plane/requestor-tenant-derivation/external-client-scoped-through-its-bridge-channel
// scenario: quic-connector/identity-published-trust-anchors-and-runtime-refresh/live-connections-survive-revocation-until-re-auth
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the identity, discovery, connector, bridge and control-plane guests built for wasm32-unknown-unknown"]
async fn identity_spine_onboards_and_reaches_the_control_plane() {
    // The connector binds a fixed listener; hold the port guard for the
    // whole test so the no-mTLS test cannot race it.
    let _port = SpinePortGuard::acquire();

    let state = StateRoot::new("identity");
    let runtime = runtime_in_state_root(&state);
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
            system_guests: vec![
                discovery_descriptor(read_wasm("selium-discovery", "selium_discovery.wasm")),
                store_descriptor(vec!["discovery".to_string()]),
                identity_descriptor(read_wasm("selium-identity", "selium_identity.wasm")),
                operator_descriptor(
                    read_wasm("selium-identity-onboard", "selium_identity_onboard.wasm"),
                    client_spki.clone(),
                    MODE_ONBOARD,
                    vec![
                        "discovery".to_string(),
                        "store".to_string(),
                        "identity".to_string(),
                    ],
                ),
                connector_descriptor(
                    read_wasm("selium-connector-quic", "selium_connector_quic.wasm"),
                    vec![
                        "discovery".to_string(),
                        "store".to_string(),
                        "identity".to_string(),
                    ],
                ),
                bridge_server_descriptor(
                    read_wasm("selium-bridge", "selium_bridge.wasm"),
                    read_wasm("selium-bridge-channel", "selium_bridge_channel.wasm"),
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
            state_dir: state.config_dir(),
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
        CONVERGENCE_BOUND,
    );

    // The bridge publishes its route from its own entrypoint; a client that
    // connects before that would be refused by the connector with "no route".
    let _ = wait_for_logs(
        &runtime,
        bridge,
        &[("bridge-server: route registered", 1)],
        CONVERGENCE_BOUND,
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
                    for message in drain_logs_if_live(&runtime, process_id) {
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

    // CLI subprocess spine (task 10.3): the real `sel` binary runs against
    // the same connector, bridge, and control plane. Each invocation is a
    // fresh process that opens one connection, issues exactly one request,
    // renders one line, and exits — so state crossing invocations can only
    // come from the control plane's durable projection.
    let select_binary = || -> PathBuf {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace.join("target"));
        // The in-process guests only progress while this runtime's threads
        // run, so hand the blocking subprocess to `block_in_place` rather
        // than parking an async worker on it.
        let output = tokio::task::block_in_place(|| {
            Command::new(cargo)
                .current_dir(&workspace)
                .args(["build", "-p", "selium-cli"])
                .output()
                .expect("run cargo to build the sel binary")
        });
        assert!(
            output.status.success(),
            "failed to build sel: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        target.join("debug").join("sel")
    };
    let binary = select_binary();

    let credentials = std::env::temp_dir().join(format!(
        "selium-cli-spine-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&credentials).expect("create CLI credential directory");
    let ca_path = credentials.join("ca.pem");
    let cert_path = credentials.join("client-cert.pem");
    let key_path = credentials.join("client-key.pem");
    std::fs::write(&ca_path, BRIDGE_CERT_PEM).expect("write CA PEM");
    let leaf = read_issued_leaf(&runtime);
    std::fs::write(&cert_path, pem_encode("CERTIFICATE", &leaf))
        .expect("write client certificate PEM");
    std::fs::write(&key_path, client_key.serialize_pem()).expect("write client key PEM");

    let invoke = |arguments: &[&str]| {
        tokio::task::block_in_place(|| {
            Command::new(&binary)
                .args([
                    "--connector",
                    CONNECTOR_ADDR,
                    "--ca",
                    ca_path.to_str().expect("CA path"),
                    "--client-cert",
                    cert_path.to_str().expect("certificate path"),
                    "--client-key",
                    key_path.to_str().expect("key path"),
                ])
                .args(arguments)
                .output()
                .expect("run the sel binary")
        })
    };

    // The CLI is the first external client: one process, one connection, one
    // request. The bridge and control plane converge asynchronously (the
    // suite's other clients wait through `wait_for_logs`); the CLI itself is
    // synchronous by design, so the harness waits for convergence by retrying
    // only connection-level failures.
    // Connection-level failures are convergence, not a typed CLI result. Retry
    // those within the same bounded window the other external clients use; a
    // typed response (including not-found) is returned immediately.
    let invoke_ready = |arguments: &[&str]| {
        let mut attempts = 0;
        loop {
            let output = invoke(arguments);
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let not_ready = stderr.contains("connection lost") || stderr.contains("timed out");
            if not_ready && attempts < 8 {
                attempts += 1;
                tokio::task::block_in_place(|| std::thread::sleep(Duration::from_secs(5)));
                continue;
            }
            break output;
        }
    };
    let missing = invoke_ready(&["status", "absent"]);
    assert!(
        !missing.status.success(),
        "status for an unknown workload must exit non-zero; stdout: {}; stderr: {}; state: {:#?}",
        String::from_utf8_lossy(&missing.stdout),
        String::from_utf8_lossy(&missing.stderr),
        dump(),
    );
    assert_eq!(
        String::from_utf8_lossy(&missing.stdout).trim(),
        "not found",
        "stderr: {}",
        String::from_utf8_lossy(&missing.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&missing.stdout).lines().count(),
        1,
        "one invocation renders one line"
    );
    assert!(
        missing.stderr.is_empty(),
        "a typed not-found response is stdout, not a transport diagnostic: {}",
        String::from_utf8_lossy(&missing.stderr)
    );

    // mTLS handshake with the identity-issued leaf against the wasm connector.
    let connecting = selium_client::connect(
        CONNECTOR_ADDR.parse().expect("connector address"),
        client_options(leaf.clone(), client_pkcs8.clone()),
    );
    let client = match tokio::time::timeout(HANDSHAKE_BOUND, connecting).await {
        Ok(result) => result.expect("client connection"),
        Err(_elapsed) => panic!("handshake timed out; state: {:#?}", dump()),
    };

    // The bridge creates a fresh channel for every stream; connection-level
    // loss during that convergence is retried exactly like the CLI subprocess.
    // A typed channel rejection remains fatal immediately.
    let mut attempts = 0;
    let mut rpc = loop {
        let opening = client.rpc::<ControlRequest, ControlResponse>(CONTROL_URI);
        match tokio::time::timeout(CONVERGENCE_BOUND, opening).await {
            Ok(Ok(result)) => break result,
            Ok(Err(error)) if attempts < 8 => {
                eprintln!("control channel not ready ({attempts}): {error}");
                attempts += 1;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            Ok(Err(error)) => panic!(
                "control route channel open failed: {error}; state: {:#?}",
                dump()
            ),
            Err(_elapsed) if attempts < 8 => {
                eprintln!("control channel open timed out ({attempts})");
                attempts += 1;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            Err(_elapsed) => panic!("channel open timed out; state: {:#?}", dump()),
        }
    };

    let deploying = rpc.request(ControlRequest::Deploy {
        workload_id: "api".to_string(),
        replicas: 3,
        module: "api/v1".to_string(),
    });
    let accepted = match tokio::time::timeout(CONVERGENCE_BOUND, deploying).await {
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

    let successful = invoke_ready(&["status", "api"]);
    assert!(
        successful.status.success(),
        "a recorded workload exits zero; stdout: {}; stderr: {}; state: {:#?}",
        String::from_utf8_lossy(&successful.stdout),
        String::from_utf8_lossy(&successful.stderr),
        dump(),
    );
    assert_eq!(
        String::from_utf8_lossy(&successful.stdout),
        "workload api: 3 replicas, module api/v1\n",
        "the successful response is rendered exactly on stdout"
    );
    assert!(
        successful.stderr.is_empty(),
        "a successful request emits no stderr diagnostic: {}",
        String::from_utf8_lossy(&successful.stderr)
    );
    drop(std::fs::remove_dir_all(&credentials));

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
            system_guests: vec![operator_descriptor(
                read_wasm("selium-identity-onboard", "selium_identity_onboard.wasm"),
                Vec::new(),
                MODE_REVOKE,
                // A second `bootstrap_system_guests` call validates
                // dependencies only against its own guest list, so the
                // already-running discovery/identity are not listed.
                Vec::new(),
            )],
            state_dir: state.config_dir(),
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
        OBSERVATION_BOUND,
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
    let refused_client = match tokio::time::timeout(HANDSHAKE_BOUND, refused).await {
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
    match tokio::time::timeout(CONVERGENCE_BOUND, opening).await {
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
        let logs = drain_logs_if_live(&runtime, process_id);
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
                            || message.contains("no client trust anchors configured")
                            || message.contains("accept_bi failed: closed by peer:")))
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
    // The revocation driver is a one-shot guest: it completes and is reaped by
    // the runtime, so there is nothing left to stop.
    drop(revoke_guest);
}

/// Encodes DER bytes as a PEM block (the CLI reads certificate and key files
/// as PEM, while the identity guest returns the issued leaf as DER).
fn pem_encode(label: &str, der: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut body = String::new();
    for chunk in der.chunks(48) {
        let mut line = String::new();
        for triple in chunk.chunks(3) {
            let first = u32::from(triple.first().copied().unwrap_or(0));
            let second = u32::from(triple.get(1).copied().unwrap_or(0));
            let third = u32::from(triple.get(2).copied().unwrap_or(0));
            let value = (first << 16) | (second << 8) | third;
            let digit = |shift: u32| -> char {
                let index = ((value >> shift) & 0x3f) as usize;
                ALPHABET.get(index).map(|byte| *byte as char).unwrap_or('=')
            };
            line.push(digit(18));
            line.push(digit(12));
            line.push(if triple.len() > 1 { digit(6) } else { '=' });
            line.push(if triple.len() > 2 { digit(0) } else { '=' });
        }
        body.push_str(&line);
        body.push('\n');
    }
    format!("-----BEGIN {label}-----\n{body}-----END {label}-----\n")
}
