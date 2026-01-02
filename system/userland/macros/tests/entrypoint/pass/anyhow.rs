#![allow(unused)]

use anyhow::Result;
use selium_userland_macros::entrypoint;

#[entrypoint]
fn guest() -> Result<()> {
    Ok(())
}

fn main() {}
