use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::{LitByteStr, LitStr, parse_macro_input};

pub fn expand(item: TokenStream) -> TokenStream {
    let lit = parse_macro_input!(item as LitStr);
    let hash = blake3::hash(lit.value().as_bytes());
    let hash_bytes = &hash.as_bytes()[0..16];
    let hash_lit = LitByteStr::new(hash_bytes, Span::call_site());

    quote! {
        selium_userland::DependencyId(*#hash_lit)
    }
    .into()
}
