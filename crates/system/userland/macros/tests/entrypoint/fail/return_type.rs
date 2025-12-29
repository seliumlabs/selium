#![allow(unused)]

use selium_userland_macros::entrypoint;

#[entrypoint]
fn guest() -> u32 {
    1
}

fn main() {}
