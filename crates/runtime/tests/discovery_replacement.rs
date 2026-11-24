//! Discovery replacement and identity domain-replay integration.
//!
//! The identity guest keeps running while the real discovery guest and its
//! listener are replaced. Its next reconciliation pass must connect to the
//! replacement listener, not the startup queue, and republish the durable
//! domain registry there.

#![allow(clippy::panic, reason = "integration-test assertions")]

use selium_abi::{Capability, CapabilityGrant, ResourceClass, ResourceSelector};
use selium_runtime::{Runtime, RuntimeConfig, SystemGuestArg, SystemGuestDescriptor};
use spine_common::{
    CONVERGENCE_BOUND, StateRoot, discovery_descriptor, identity_descriptor, read_wasm,
    runtime_in_state_root, store_descriptor, wait_for_logs,
};

mod common;
mod spine_common;

const DOMAIN: &str = "replacement.example";
const TENANT: &str = "acme";

// scenario: addressing/advisory-domain-to-tenant-table/mapping-survives-restart
// scenario: selium-identity/durable-state-and-replay/restart-replays-registries
#[test]
#[ignore = "requires discovery, store, identity and discovery-probe WASM guests"]
fn identity_domains_converge_on_a_replacement_discovery_listener() {
    let state = StateRoot::new("discovery-replacement");
    let runtime = runtime_in_state_root(&state);
    seed_domain(&runtime);

    let first = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: vec![
                discovery_descriptor(read_wasm("selium-discovery", "selium_discovery.wasm")),
                store_descriptor(vec!["discovery".to_string()]),
                identity_descriptor(read_wasm("selium-identity", "selium_identity.wasm")),
            ],
            state_dir: state.config_dir(),
        })
        .expect("bootstrap original discovery and identity");
    let original = find(&first, "discovery");
    let identity = find(&first, "identity");
    let original_listener = runtime
        .discovery_listener_shared_id()
        .expect("original discovery listener");
    let original_domains = probe(&runtime, "original-domains", "", 13);
    wait_for_logs(
        &runtime,
        original_domains,
        &[(&format!("domains: {DOMAIN}={TENANT}"), 1)],
        CONVERGENCE_BOUND,
    );
    runtime
        .stop_process(original_domains)
        .expect("stop original domains probe");

    runtime.stop_process(original).expect("stop discovery");
    let replacement = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: vec![discovery_descriptor(read_wasm(
                "selium-discovery",
                "selium_discovery.wasm",
            ))],
            state_dir: state.config_dir(),
        })
        .expect("bootstrap replacement discovery");
    let replacement = find(&replacement, "discovery");
    let replacement_listener = runtime
        .discovery_listener_shared_id()
        .expect("replacement discovery listener");
    assert_ne!(replacement_listener, original_listener);

    // The published route resolves to the actual replacement queue, rather
    // than merely updating runtime-private bootstrap state.
    let resolver = probe(&runtime, "discovery-route-resolver", "sel:///discovery", 3);
    wait_for_logs(
        &runtime,
        resolver,
        &[(
            &format!("resolver: found sel:///discovery {replacement_listener}"),
            1,
        )],
        CONVERGENCE_BOUND,
    );

    // Identity never restarted. Its durable registry must arrive through a
    // fresh discovery session to the replacement listener.
    assert!(runtime.is_guest_resident(identity));
    let domains = probe(&runtime, "replacement-domains", "", 13);
    wait_for_logs(
        &runtime,
        domains,
        &[(&format!("domains: {DOMAIN}={TENANT}"), 1)],
        CONVERGENCE_BOUND,
    );

    drop(runtime.stop_process(domains));
    drop(runtime.stop_process(replacement));
    drop(runtime.stop_process(identity));
    for guest in first.guests {
        drop(runtime.stop_process(guest.process_id));
    }
}

#[test]
#[ignore = "requires discovery and discovery-probe WASM guests with the connector resolvers"]
fn connector_resolvers_converge_on_a_replacement_discovery_listener() {
    const PROVIDER_ROLE: u64 = 14;
    const RESOLVER_ROLE: u64 = 15;

    let state = StateRoot::new("connector-discovery-replacement");
    let runtime = runtime_in_state_root(&state);
    let first = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: vec![discovery_descriptor(read_wasm(
                "selium-discovery",
                "selium_discovery.wasm",
            ))],
            state_dir: state.config_dir(),
        })
        .expect("bootstrap original discovery");
    let original = find(&first, "discovery");
    let original_listener = runtime
        .discovery_listener_shared_id()
        .expect("original discovery listener");

    let http_provider = probe_with_claims(
        &runtime,
        "original-http-provider",
        "http",
        PROVIDER_ROLE,
        &["sel:///http"],
    );
    let quic_provider = probe_with_claims(
        &runtime,
        "original-quic-provider",
        "quic",
        PROVIDER_ROLE,
        &["sel:///quic"],
    );
    for (provider, route) in [(http_provider, "http"), (quic_provider, "quic")] {
        let registered = format!("connector-provider: registered sel:///{route} {provider}");
        wait_for_logs(
            &runtime,
            provider,
            &[(registered.as_str(), 1)],
            CONVERGENCE_BOUND,
        );
    }

    let resolver = probe(&runtime, "connector-resolvers", "", RESOLVER_ROLE);
    wait_for_logs(
        &runtime,
        resolver,
        &[
            (
                format!("connector-resolver: http initial {http_provider}").as_str(),
                1,
            ),
            (
                format!("connector-resolver: quic initial {quic_provider}").as_str(),
                1,
            ),
        ],
        CONVERGENCE_BOUND,
    );

    runtime.stop_process(original).expect("stop discovery");
    runtime
        .stop_process(http_provider)
        .expect("stop original HTTP provider");
    runtime
        .stop_process(quic_provider)
        .expect("stop original QUIC provider");

    let replacement = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: vec![discovery_descriptor(read_wasm(
                "selium-discovery",
                "selium_discovery.wasm",
            ))],
            state_dir: state.config_dir(),
        })
        .expect("bootstrap replacement discovery");
    let replacement_discovery = find(&replacement, "discovery");
    let replacement_listener = runtime
        .discovery_listener_shared_id()
        .expect("replacement discovery listener");
    assert_ne!(replacement_listener, original_listener);
    assert!(runtime.is_guest_resident(resolver));

    let http_replacement = probe_with_claims(
        &runtime,
        "replacement-http-provider",
        "http",
        PROVIDER_ROLE,
        &["sel:///http"],
    );
    let quic_replacement = probe_with_claims(
        &runtime,
        "replacement-quic-provider",
        "quic",
        PROVIDER_ROLE,
        &["sel:///quic"],
    );
    wait_for_logs(
        &runtime,
        resolver,
        &[
            (
                format!("connector-resolver: http recovered {http_replacement}").as_str(),
                1,
            ),
            (
                format!("connector-resolver: quic recovered {quic_replacement}").as_str(),
                1,
            ),
        ],
        CONVERGENCE_BOUND,
    );

    drop(runtime.stop_process(resolver));
    drop(runtime.stop_process(http_replacement));
    drop(runtime.stop_process(quic_replacement));
    drop(runtime.stop_process(replacement_discovery));
}

fn probe(runtime: &Runtime, name: &str, route: &str, role: u64) -> u64 {
    probe_with_claims(runtime, name, route, role, &[])
}

fn probe_with_claims(
    runtime: &Runtime,
    name: &str,
    route: &str,
    role: u64,
    claims: &[&str],
) -> u64 {
    let mut grants = vec![
        CapabilityGrant::new(
            Capability::HostQueue,
            vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
        ),
        CapabilityGrant::new(
            Capability::SharedMemory,
            vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
        ),
    ];
    grants.extend(claims.iter().map(|uri| {
        CapabilityGrant::new(
            Capability::RouteClaim,
            vec![ResourceSelector::Uri((*uri).to_string())],
        )
    }));
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: false,
            system_guests: vec![SystemGuestDescriptor {
                name: name.to_string(),
                module_id: format!("{name}-module"),
                module_bytes: read_wasm("selium-discovery-probe", "selium_discovery_probe.wasm"),
                entrypoint: "discovery_probe".to_string(),
                arguments: vec![
                    SystemGuestArg::Pointer(route.as_bytes().to_vec()),
                    SystemGuestArg::Integer(role),
                ],
                grants,
                tenant: None,
                discovery_service: false,
                handlers: Vec::new(),
            }],
            state_dir: None,
        })
        .expect("bootstrap discovery probe");
    find(&report, name)
}

fn seed_domain(runtime: &Runtime) {
    let payload = selium_abi::encode_rkyv(&selium_identity::DomainRecord::Set {
        domain: DOMAIN.to_string(),
        tenant: TENANT.to_string(),
    })
    .expect("encode domain record");
    let record = selium_store::format::Record {
        sequence: 1,
        timestamp_ms: 1,
        headers: Vec::new(),
        payload,
    };
    let framed = selium_store::format::frame(&selium_store::format::encode_record(&record));
    let volumes = runtime.kernel().volumes();
    let volume = volumes
        .open(
            None,
            &selium_store::format::log_volume(selium_identity::DOMAIN_LOG, 0),
        )
        .expect("open domain log");
    volumes
        .append(volume.local_id, &framed)
        .expect("append domain record");
    volumes.sync(volume.local_id).expect("sync domain record");
}

fn find(report: &selium_runtime::BootstrapReport, name: &str) -> u64 {
    report
        .guests
        .iter()
        .find(|guest| guest.name == name)
        .unwrap_or_else(|| panic!("bootstrap report contains {name}"))
        .process_id
}
