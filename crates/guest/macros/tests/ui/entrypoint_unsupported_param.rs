use selium_guest::entrypoint;

fn main() {}

#[entrypoint]
async fn unsupported_param_entrypoint(name: String) {}
