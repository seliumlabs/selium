use selium_guest::{EntrypointMetadata, entrypoint};
use thiserror::Error;

#[derive(Debug, Error)]
#[error("{0}")]
struct TestError(String);

#[test]
fn entrypoint_with_result_generates_metadata_and_returns_i32() {
    let entrypoint_metadata: EntrypointMetadata = result_entrypoint_entrypoint_metadata();
    assert_eq!(entrypoint_metadata.name, "result_entrypoint");

    // Verify the extern "C" fn exists and returns i32 = 0 on Ok(())
    let result = __selium_guest_entrypoint_result_entrypoint();
    assert_eq!(result, 0);
}

#[entrypoint]
async fn result_entrypoint() -> Result<(), TestError> {
    tracing::info!("result entrypoint invoked");
    Ok(())
}
