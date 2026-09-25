// Re-export generated FlatBuffers bindings.
// The generated file wraps everything in `pub mod selium { pub mod dns { ... } }`.
#[rustfmt::skip]
include!("selium/dns/dns_generated.rs");
