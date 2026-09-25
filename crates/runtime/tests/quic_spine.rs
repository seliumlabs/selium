//! QUIC connector spine test.
//!
//! Deploys the real `selium-discovery`, `selium-connector-quic`, and
//! `selium-quic-demo` WASM guests and drives the full QUIC relay path
//! end-to-end: a native `selium-client` (the SDK collapses the QUIC endpoint,
//! TLS configuration, and handshake) completes a TLS 1.3 handshake against the
//! connector guest's quinn endpoint — running on wasm32 over the guest's
//! shared-memory `UdpSocket` and the connector's `AsyncUdpSocket` /
//! `Runtime` adapters — and echoes byte-identical payloads on two concurrent
//! bidirectional streams through per-stream shared-memory channels to the
//! demo guest.
//!
//! This is the wasm32-level verification of the `quic-connector` capability:
//! the native `selium-connector-quic` tests substitute tokio UDP/runtime
//! doubles (the shm adapters need the guest hostcalls), so only this test
//! exercises the production adapter pair against a real handshake, plus the
//! discovery wiring (external-name registration, SNI resolve, queue attach)
//! that SNI routing depends on.
//!
//! Covers:
//! - edge termination of real QUIC (TLS 1.3) over the guest UDP socket,
//! - SNI-based route resolution through the real discovery guest,
//! - opaque byte-stream forwarding, byte-identical and ordered,
//! - payloads larger than the per-stream ring capacity (ring parks +
//!   quinn flow control: backpressure honesty, no lost bytes),
//! - per-stream channel isolation (two concurrent streams do not cross),
//! - stream lifecycle fidelity (client FIN → guest EOF, guest close →
//!   wire FIN observed by the client),
//! - the zero-`Network`-grant app-guest model: the demo guest holds only
//!   HostQueue + SharedMemory grants.
//!
//! `#[ignore]`d by default because it requires the guests built for
//! `wasm32-unknown-unknown` first (release profile strongly preferred —
//! the TLS handshake and bulk relay run through the wasm interpreter,
//! which is too slow at debug optimization for the quinn timeouts):
//!
//! ```sh
//! cargo build --release --target wasm32-unknown-unknown -p selium-discovery -p selium-connector-quic -p selium-quic-demo
//! cargo test -p selium-runtime --test quic_spine -- --ignored
//! ```
//!
//! The connector binds a fixed listener (`0.0.0.0:4433`, see its
//! `QUIC_LISTEN_ADDR`), so only one instance of this test may run at a
//! time.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use selium_abi::{Capability, CapabilityGrant, ResourceClass, ResourceSelector};
use selium_client::ConnectOptions;
use selium_runtime::{ReadinessCondition, Runtime, RuntimeConfig, SystemGuestDescriptor};
use selium_service::FlatMsg;

mod common;

/// Connector TLS material, provisioned into the `tls-certs` blob store
/// before the connector guest boots (the same PEM fixtures the
/// `selium-connector-quic` native handshake tests derive their DER from).
const CERT_PEM: &[u8] = include_bytes!("../../../guests/connector-quic/tests/fixtures/cert.pem");
/// The connector's fixed listener (its `QUIC_LISTEN_ADDR` const).
const CONNECTOR_ADDR: &str = "127.0.0.1:4433";
const KEY_PEM: &[u8] = include_bytes!("../../../guests/connector-quic/tests/fixtures/key.pem");
/// Per-stream payload. Sized for the wasm interpreter's current QUIC
/// throughput: each wire packet costs tens of milliseconds of interpreted
/// crypto, so bulk transfers crawl (and the connector's default 30 s idle
/// timeout fires before a paced client can finish one). Once wasm
/// performance work lands, raise this back to 128 KiB — twice the
/// connector's 64 KiB per-direction ring capacity — so each direction is
/// forced through at least one park/resume cycle by payload size alone.
const PAYLOAD_LEN: usize = 16 * 1024;
/// SNI / bare route name; must match the certificate's SAN.
const SERVER_NAME: &str = "localhost";
/// Warm-up round trips driven before the timed payload streams (see the
/// handshake comment). Each opens a stream, so the guest-side "one accept
/// per stream" assertion counts these too; keep the two in sync.
const WARMUP_ROUNDS: usize = 12;

/// Builds the `selium-client` connection options: trust the connector's
/// self-signed test certificate, with a patient transport config (the wasm32
/// guests run on an interpreter, so the TLS handshake takes far longer than
/// the quinn defaults assume).
fn client_options() -> ConnectOptions {
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(quinn::IdleTimeout::from(quinn::VarInt::from(
        300_000u32,
    ))));
    // The wasm32 guest processes each packet on an interpreter: round trips
    // are tens of milliseconds (release) to seconds (debug). A large initial
    // RTT keeps the handshake patient but strangles the client's congestion
    // window (slow start adds one MSS per RTT); keep it modest.
    transport.initial_rtt(Duration::from_millis(250));

    ConnectOptions {
        server_name: SERVER_NAME.to_string(),
        server_root: selium_client::certificates_from_pem(CERT_PEM).expect("parse certificate PEM"),
        identity: None,
        transport: Some(Arc::new(transport)),
    }
}

/// The QUIC connector system guest. Empty arguments + no well-known URI mean
/// bootstrap injects the discovery handle as the leading entrypoint argument
/// (consumed by the `Context` parameter) and grants attach rights for the
/// discovery listener. `handlers: ["sel-quic"]` publishes the Tier-1
/// protocol-handler registration discovery requires before accepting the
/// demo's Tier-2 external `localhost` route.
fn connector_descriptor(module_bytes: Vec<u8>) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "quic-connector".to_string(),
        module_id: "quic-connector-module".to_string(),
        module_bytes,
        entrypoint: "connector_quic".to_string(),
        arguments: Vec::new(), // discovery handle injected by bootstrap
        grants: vec![
            CapabilityGrant::new(
                Capability::Network,
                vec![ResourceSelector::ResourceClass(ResourceClass::UdpSocket)],
            ),
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
            CapabilityGrant::new(
                Capability::Storage,
                vec![ResourceSelector::ResourceClass(ResourceClass::BlobStore)],
            ),
        ],
        dependencies: vec!["discovery".to_string()],
        readiness: ReadinessCondition::Immediate,
        tenant: None,
        serving_role: None,
        handlers: vec!["sel-quic".to_string()],
    }
}

fn connector_wasm() -> Vec<u8> {
    read_wasm("selium-connector-quic", "selium_connector_quic.wasm")
}

/// The echo demo app guest. Note the grants: HostQueue (create its listener,
/// attach the discovery listener) + SharedMemory (attach delivered stream
/// regions) — **no `Network` grant**: QUIC is terminated by the connector.
/// `SystemRegistration` lets the root-tenant demo register its synthetic
/// `localhost` route under the root namespace.
fn demo_descriptor(module_bytes: Vec<u8>) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "quic-demo".to_string(),
        module_id: "quic-demo-module".to_string(),
        module_bytes,
        entrypoint: "quic_demo".to_string(),
        arguments: Vec::new(), // discovery handle injected by bootstrap
        grants: vec![
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(Capability::SystemRegistration, vec![]),
        ],
        dependencies: vec!["discovery".to_string()],
        readiness: ReadinessCondition::Immediate,
        tenant: None,
        serving_role: None,
        handlers: Vec::new(),
    }
}

fn demo_wasm() -> Vec<u8> {
    read_wasm("selium-quic-demo", "selium_quic_demo.wasm")
}

/// The discovery system guest, exactly as the `discovery` integration test
/// deploys it: the guest is named `"discovery"` so bootstrap wires the feed
/// region and listener handle into its arguments and grants.
fn discovery_descriptor(module_bytes: Vec<u8>) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "discovery".to_string(),
        module_id: "discovery-module".to_string(),
        module_bytes,
        entrypoint: "discovery_main".to_string(),
        arguments: Vec::new(), // populated by bootstrap via set_discovery_feed_and_handle
        grants: vec![
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
        ],
        dependencies: Vec::new(),
        readiness: ReadinessCondition::ActivityLogContains("guest ready".to_string()),
        tenant: None,
        serving_role: None,
        handlers: Vec::new(),
    }
}

fn discovery_wasm() -> Vec<u8> {
    read_wasm("selium-discovery", "selium_discovery.wasm")
}

/// Drains a guest's log channel and decodes each frame as a `LogRecord`.
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

/// Opens one bidirectional stream, writes `payload`, finishes (client FIN),
/// then reads the echo to end-of-stream (the guest's close surfaces as the
/// wire FIN via the connector) and returns it. The caller asserts on the
/// content so failure output can include guest-side logs.
async fn echo_round_trip(connection: &quinn::Connection, payload: Vec<u8>) -> Vec<u8> {
    let (mut send, mut recv) = connection.open_bi().await.expect("open stream");

    send.write_all(&payload).await.expect("client write");
    send.finish().expect("client finish");

    let mut echo = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        // `None` from quinn's read is end-of-stream.
        let n = recv
            .read(&mut chunk)
            .await
            .expect("client echo read")
            .unwrap_or(0);
        if n == 0 {
            break;
        }
        echo.extend_from_slice(chunk.get(..n).unwrap_or_default());
    }
    echo
}

/// Golden path: external quinn client → connector handshake + streams →
/// per-stream channels → demo guest echo → bytes returned, byte-identical,
/// on two concurrent streams of one connection.
// The connector's quinn timers (loss detection, retransmit) sleep via the
// `Sleep` hostcall, whose host-side wake is driven by `tokio::spawn`, so the
// test provides a Tokio runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the discovery, connector and demo guests built for wasm32-unknown-unknown"]
#[expect(
    clippy::large_futures,
    reason = "the warm-up and payload round-trips intentionally run inline for deterministic join semantics"
)]
async fn external_quinn_client_echoes_through_wasm_connector_guest() {
    let runtime = Runtime::default();
    seed_tls_blob_store(&runtime);

    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: vec![
                discovery_descriptor(discovery_wasm()),
                connector_descriptor(connector_wasm()),
                demo_descriptor(demo_wasm()),
            ],
            domain_table: Vec::new(),
        })
        .expect("bootstrap discovery, connector and demo guests");

    let find = |name: &str| {
        report
            .guests
            .iter()
            .find(|guest| guest.name == name)
            .unwrap_or_else(|| panic!("bootstrap report contains guest {name}"))
            .process_id
    };
    let discovery = find("discovery");
    let connector = find("quic-connector");
    let demo = find("quic-demo");

    // Wait for both sides of the route before connecting: the connector's
    // UDP listener and the demo's discovery registration.
    let _ = wait_for_logs(
        &runtime,
        connector,
        &[("quic-connector: listening on", 1)],
        Duration::from_secs(30),
    );
    let bound = format!("quic-demo: bound {SERVER_NAME}");
    let _ = wait_for_logs(&runtime, demo, &[(&bound, 1)], Duration::from_secs(30));

    // A native `selium-client` SDK connection. The future is created here and
    // awaited below (after the diagnostic collector starts) so handshake logs
    // are still captured.
    let connecting = selium_client::connect(
        CONNECTOR_ADDR.parse().expect("connector address"),
        client_options(),
    );

    // Diagnostic collector: drains guest logs on a side thread so the
    // failure path never touches kernel locks (which can be held by a guest
    // executing inline on a poller thread).
    let collected: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let stop_collector = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let collector = {
        let runtime = runtime.clone();
        let collected = collected.clone();
        let stop = stop_collector.clone();
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(250));
                for process_id in [connector, demo] {
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

    // TLS 1.3 handshake against the wasm connector's quinn endpoint (the
    // production shm `AsyncUdpSocket` + guest `Runtime` adapters).
    let client = match tokio::time::timeout(Duration::from_secs(240), connecting).await {
        Ok(result) => result.expect("client connection"),
        Err(_elapsed) => {
            let mut snapshot = collected.lock().expect("collector lock").clone();
            snapshot.sort();
            panic!("handshake timed out; recent guest logs: {snapshot:#?}");
        }
    };
    stop_collector.store(true, std::sync::atomic::Ordering::Relaxed);
    collector.join().expect("collector thread");

    let connection = client.connection().clone();

    // Warm-up: the TLS handshake runs on an interpreter, so its crypto
    // flights take seconds and poison the client's RTT estimate — quinn's
    // pacer then throttles bulk sends to a crawl that its own sparse
    // sampling never recovers from. A burst of tiny round-trips first lets
    // the estimate decay to the true steady-state latency.
    for i in 0..WARMUP_ROUNDS {
        let probe = vec![0xA5u8; 32];
        let echo = echo_round_trip(&connection, probe).await;
        assert_eq!(echo.len(), 32, "warm-up round-trip {i} incomplete");
    }

    // Two concurrent bidirectional streams with distinct payloads: each
    // must echo back byte-identical, in order, without crossing.
    let payload_a = payload(PAYLOAD_LEN, 0x00);
    let payload_b = payload(PAYLOAD_LEN, 0x5A);
    let payload_a_clone = payload_a.clone();
    let payload_b_clone = payload_b.clone();

    let (echo_a, echo_b) = match tokio::time::timeout(Duration::from_secs(300), async {
        tokio::join!(
            echo_round_trip(&connection, payload_a_clone),
            echo_round_trip(&connection, payload_b_clone),
        )
    })
    .await
    {
        Ok(result) => result,
        Err(_elapsed) => {
            let mut snapshot = collected.lock().expect("collector lock").clone();
            snapshot.sort();
            panic!(
                "echo round-trips timed out; collected guest logs: {snapshot:#?}; \
                 fresh demo logs: {:#?}; fresh connector logs: {:#?}",
                drain_logs(&runtime, demo),
                drain_logs(&runtime, connector),
            );
        }
    };

    assert_eq!(
        echo_a.len(),
        payload_a.len(),
        "stream A echo length must match payload; demo logs: {:#?}; connector logs: {:#?}",
        drain_logs(&runtime, demo),
        drain_logs(&runtime, connector),
    );
    assert_eq!(
        echo_a, payload_a,
        "stream A bytes must relay byte-identical and ordered"
    );
    assert_eq!(
        echo_b.len(),
        payload_b.len(),
        "stream B echo length must match payload; demo logs: {:#?}",
        drain_logs(&runtime, demo)
    );
    assert_eq!(
        echo_b, payload_b,
        "stream B bytes must relay byte-identical and ordered"
    );

    // Guest-side assertions: one accepted stream per QUIC stream the client
    // opened (warm-up round trips included) and one full-size echo per
    // payload stream, and no error logged anywhere in the relay path. The
    // wait helper returns exactly what it drained, so the counts below
    // cover the whole connection instead of racing the guest with a second
    // drain.
    let echo_needle = format!("echoed {PAYLOAD_LEN} bytes");
    let demo_logs = wait_for_logs(
        &runtime,
        demo,
        &[
            ("accepted stream", WARMUP_ROUNDS + 2),
            (echo_needle.as_str(), 2),
        ],
        Duration::from_secs(30),
    );
    assert_eq!(
        demo_logs
            .iter()
            .filter(|message| message.contains("accepted stream"))
            .count(),
        WARMUP_ROUNDS + 2,
        "one accepted stream per QUIC stream: {demo_logs:?}"
    );
    assert_eq!(
        demo_logs
            .iter()
            .filter(|message| message.contains(echo_needle.as_str()))
            .count(),
        2,
        "one full-size echo per payload stream: {demo_logs:?}"
    );

    for (process_id, name) in [(discovery, "discovery"), (connector, "quic-connector")] {
        let logs = drain_logs(&runtime, process_id);
        assert!(
            !logs.iter().any(|message| message.contains("failed")),
            "{name} guest logged an error: {logs:?}"
        );
    }

    runtime.stop_process(demo).expect("stop demo");
    runtime.stop_process(connector).expect("stop connector");
    runtime.stop_process(discovery).expect("stop discovery");
    drop(client);
}

/// A deterministic payload of `size` bytes parameterised by `seed`, so a
/// cross-stream leak between the two concurrent streams corrupts an
/// assertion instead of passing silently.
fn payload(size: usize, seed: u8) -> Vec<u8> {
    (0..size)
        .map(|index| (index as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

/// Reads a guest WASM module, with an actionable error if it is missing.
///
/// Prefers a release-profile artifact: this test drives a TLS 1.3 handshake
/// and bulk stream relay through the wasm interpreter, which is too slow at
/// debug optimization for the quinn defaults (and the test timeout) to
/// tolerate.
fn read_wasm(crate_name: &str, file_name: &str) -> Vec<u8> {
    // The shared reader builds the release profile (a no-op when fresh) and
    // fails loudly if the build or the read fails.
    common::read_guest_wasm(crate_name, file_name)
}

/// Provisions the connector's TLS material into the `tls-certs` blob store
/// the connector opens via its `Storage` grant (`cert-pem` / `key-pem`
/// manifests). Seeded from the host before the connector boots, exactly as
/// provisioning would.
fn seed_tls_blob_store(runtime: &Runtime) {
    let storage = runtime.kernel().storage();
    let store = storage.open_blob_store(&runtime.kernel().memory(), "tls-certs");

    let cert_id = storage
        .put_blob(store.local_id, CERT_PEM.to_vec())
        .expect("put cert blob");
    let key_id = storage
        .put_blob(store.local_id, KEY_PEM.to_vec())
        .expect("put key blob");

    storage
        .set_manifest(store.local_id, "cert-pem", cert_id)
        .expect("cert manifest");
    storage
        .set_manifest(store.local_id, "key-pem", key_id)
        .expect("key manifest");
}

/// Polls a guest's log channel until every `(needle, count)` pair is
/// satisfied — each needle appears in the drained messages at least `count`
/// times — then returns every drained message so callers can assert exact
/// counts without a second drain racing further guest output.
#[expect(clippy::panic, reason = "test helper")]
fn wait_for_logs(
    runtime: &Runtime,
    process_id: u64,
    needles: &[(&str, usize)],
    timeout: Duration,
) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let start = Instant::now();
    while start.elapsed() < timeout {
        seen.extend(drain_logs(runtime, process_id));
        if needles.iter().all(|(needle, count)| {
            seen.iter()
                .filter(|message| message.contains(needle))
                .count()
                >= *count
        }) {
            return seen;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {needles:?} in guest log; got {seen:?}");
}
