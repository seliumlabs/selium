// Re-export generated FlatBuffers bindings.
// The generated file wraps everything in `pub mod selium { pub mod http { ... } }`.
#[rustfmt::skip]
include!("selium/http/http_generated.rs");
