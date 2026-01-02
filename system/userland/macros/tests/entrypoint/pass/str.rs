use selium_userland_macros::entrypoint;

#[entrypoint]
fn takes_str(domain: &str) {
    let _ = domain.len();
}

fn main() {}
