use selium_guest::{
    Capability, CapabilityGrant, EntrypointMetadata, InterfaceMetadata, LocalityScope,
    ResourceSelector, entrypoint, pattern_interface,
};
use selium_runtime::{ReadinessCondition, Runtime, RuntimeConfig, SystemGuestDescriptor};

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
fn descriptors_can_override_readiness_after_macro_generation() {
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
    descriptor.readiness = ReadinessCondition::ActivityLogContains("bootstrapped".to_string());

    assert!(matches!(
        descriptor.readiness,
        ReadinessCondition::ActivityLogContains(_)
    ));
}

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
        domain_table: Vec::new(),
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
            (import \"selium\" \"mark_ready\" (func $mark_ready))
            (func (export \"{entrypoint}\")
                call $mark_ready
                call $process_id
                drop))"
    ))
    .expect("compile wat")
}
