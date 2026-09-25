use selium_guest::entrypoint;

fn main() {}

#[entrypoint]
async fn pointer_entrypoint(_resolver: (u64, u64)) {}
