use selium_guest::entrypoint;

struct NotAnError;

#[entrypoint]
async fn bad_entrypoint() -> Result<(), NotAnError> {
    Ok(())
}

fn main() {}
