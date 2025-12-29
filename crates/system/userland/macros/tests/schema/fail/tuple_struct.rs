#![allow(unused)]
use selium_userland_macros::schema;

#[schema(
    path = concat!(env!("SEL_USERLAND_MACROS_DIR"), "/tests/schemas/echo.fbs"),
    ty = "selium.examples.Echo",
    binding = "crate::bindings::Echo"
)]
pub struct BadTuple(String);

mod bindings {}

fn main() {}
