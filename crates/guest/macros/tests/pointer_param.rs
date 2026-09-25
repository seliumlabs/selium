use selium_guest::{EntrypointMetadata, entrypoint};

#[test]
fn metadata_name_is_preserved() {
    let metadata: EntrypointMetadata = pointer_entrypoint_entrypoint_metadata();
    assert_eq!(metadata.name, "pointer_entrypoint");
}

#[entrypoint]
async fn pointer_entrypoint(_resolver: (u64, u64)) {}

#[test]
fn pointer_signature_exports_two_argument_slots() {
    // The generated export takes `(i64, i64)`: address then length.
    __selium_guest_entrypoint_pointer_entrypoint(0x1000, 20);
}
