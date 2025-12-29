#![allow(unused)]

use selium_userland_macros::entrypoint;

#[entrypoint]
async fn guest(mut count: i32, message: u16) -> Result<(), ()> {
    count += i32::from(message);
    Ok(())
}

fn main() {}
