#![allow(unused)]

use selium_userland_macros::entrypoint;

#[entrypoint]
fn guest((a, b): (i32, i32)) {}

fn main() {}
