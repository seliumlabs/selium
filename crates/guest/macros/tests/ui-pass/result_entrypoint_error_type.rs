//! A `Result` entrypoint whose error type implements `std::error::Error` is
//! accepted by the macro; the rejected counterpart is
//! `tests/ui/result_entrypoint_non_error.rs`.
use selium_guest::entrypoint;

#[derive(Debug)]
struct CustomError;

impl std::fmt::Display for CustomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("custom")
    }
}

impl std::error::Error for CustomError {}

fn main() {}

#[entrypoint]
async fn error_typed_entrypoint() -> Result<(), CustomError> {
    Ok(())
}
