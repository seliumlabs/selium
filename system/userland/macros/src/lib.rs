use proc_macro::TokenStream;

mod dependency_id;
mod entrypoint;
mod schema;

/// Compute a singleton dependency identifier from a string literal.
#[proc_macro]
pub fn dependency_id(item: TokenStream) -> TokenStream {
    dependency_id::expand(item)
}

/// Struct-level schema annotation declaring a message type.
#[proc_macro_attribute]
pub fn schema(attr: TokenStream, item: TokenStream) -> TokenStream {
    schema::expand(attr, item)
}

#[proc_macro_attribute]
pub fn entrypoint(attr: TokenStream, item: TokenStream) -> TokenStream {
    entrypoint::expand(attr, item)
}
