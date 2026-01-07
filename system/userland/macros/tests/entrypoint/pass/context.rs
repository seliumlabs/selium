#![allow(unused)]

use selium_userland::Context;
use selium_userland_macros::entrypoint;

#[entrypoint]
async fn guest(ctx: Context) -> Result<(), ()> {
    let _ = ctx;
    Ok(())
}

fn main() {}
