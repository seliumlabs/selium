#![allow(unused)]

use selium_userland_macros::entrypoint;

#[entrypoint]
async fn guest() -> Result<(), ()> {
    Ok(())
}

fn main() {}
