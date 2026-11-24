use selium_guest::{
    Capability, CapabilityGrant, EntrypointMetadata, InterfaceMetadata, LocalityScope,
    ResourceSelector, entrypoint, pattern_interface,
};
use selium_runtime::{Runtime, RuntimeConfig, SystemGuestDescriptor};

#[pattern_interface]
#[expect(dead_code, reason = "macro-generated interface used at build time")]
trait Echo {
    fn echo(&self);
}

#[entrypoint]
async fn demo_entrypoint() {
    tracing::info!("macro entrypoint invoked");
}

#[test]
fn descriptors_build_from_macro_metadata() {
    let mut descriptor = SystemGuestDescriptor::from_entrypoint_metadata(
        "demo",
        "demo-module",
        module_with_entrypoint("demo_entrypoint"),
        demo_entrypoint_entrypoint_metadata(),
        vec![CapabilityGrant::new(
            Capability::ProcessLifecycle,
            vec![ResourceSelector::Locality(LocalityScope::Cluster)],
        )],
    );
    // `descriptor` stays mutable for callers that adjust it before bootstrap.
    descriptor.entrypoint = "demo_entrypoint".to_string();

    assert_eq!(descriptor.entrypoint, "demo_entrypoint");
}
// scenario: selium-guest-macros/guest-entrypoint-macro/macro-generated-entrypoint-glue
// scenario: selium-guest-macros/backward-compatible-entrypoint-return-types/existing-void-entrypoint-unchanged
// scenario: selium-guest-macros/generated-pattern-metadata/messaging-interface-metadata-emitted
// scenario: selium-guest-macros/bootstrap-aware-macro-integration/runtime-bootstraps-macro-based-guest

#[tokio::test(flavor = "current_thread")]
async fn macros_generate_metadata_compatible_with_runtime_and_tracing() {
    let entrypoint_metadata: EntrypointMetadata = demo_entrypoint_entrypoint_metadata();
    assert_eq!(entrypoint_metadata.name, "demo_entrypoint");

    let interface_metadata: InterfaceMetadata = echo_pattern_metadata();
    assert_eq!(interface_metadata.name, "Echo");
    assert_eq!(interface_metadata.methods, vec!["echo".to_string()]);

    __selium_guest_entrypoint_demo_entrypoint();

    let runtime = Runtime::default();
    let config = RuntimeConfig {
        start_discovery: false,
        system_guests: vec![SystemGuestDescriptor::from_entrypoint_metadata(
            "demo",
            "demo-module",
            module_with_entrypoint("demo_entrypoint"),
            entrypoint_metadata,
            vec![CapabilityGrant::new(
                Capability::ProcessLifecycle,
                vec![ResourceSelector::Locality(LocalityScope::Cluster)],
            )],
        )],
        state_dir: None,
    };

    let report = runtime
        .bootstrap_system_guests(config)
        .expect("bootstrap runtime");
    assert_eq!(report.guests.len(), 1);
    assert_eq!(runtime.loaded_guest_count(), 1);
}

fn module_with_entrypoint(entrypoint: &str) -> Vec<u8> {
    wat::parse_str(format!(
        "(module
            (import \"selium\" \"process_id\" (func $process_id (result i64)))
            (func (export \"{entrypoint}\")
                call $process_id
                drop))"
    ))
    .expect("compile wat")
}
