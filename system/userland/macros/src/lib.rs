use proc_macro::TokenStream;

mod entrypoint;
mod schema;

/// Struct-level schema annotation declaring a message type.
#[proc_macro_attribute]
pub fn schema(attr: TokenStream, item: TokenStream) -> TokenStream {
    schema::expand(attr, item)
}

#[proc_macro_attribute]
pub fn entrypoint(attr: TokenStream, item: TokenStream) -> TokenStream {
    entrypoint::expand(attr, item)
}
