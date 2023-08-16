extern crate lazy_static;
extern crate proc_macro;
extern crate quote;
extern crate rhai;
extern crate selium_common;
extern crate syn;

use lazy_static::lazy_static;
use proc_macro::TokenStream;
use quote::quote;
use rhai::Engine;
use syn::parse_macro_input;

lazy_static! {
    static ref ENGINE: Engine = Engine::new();
}

/// Validates a [mod@rhai] expression at build time, wrapping the validated
/// expression in an [Executor](selium_common::types::Executor).
///
/// # Errors
/// 
/// If the provided expression is not valid Rhai syntax, a compiler error 
/// will be emitted at build time.
///
/// ```compile_fail
/// # #[macro_use] extern crate selium_macros;
/// use selium_macros::rhai;
///
/// # fn main() {
/// rhai!("5 @ 5");
/// # }
/// ```
///
/// # Examples
///
/// The following examples are all valid Rhai expressions, and will compile successfully.
///
/// See the [Rhai Book](https://rhai.rs/book/) for an in-depth overview of Rhai
/// syntax.
///
/// ```
/// # #[macro_use] extern crate selium_macros;
/// # extern crate selium_common;
/// use selium_macros::rhai;
/// use selium_common::types::Executor;
///
/// # fn main() {
/// assert_eq!(rhai!("5 + 5"), Executor::Rhai("5 + 5".to_owned()));
/// assert_eq!(rhai!("\"hello, world!\""), Executor::Rhai("\"hello, world!\"".to_owned()));
/// assert_eq!(rhai!("item += 8"), Executor::Rhai("item += 8".to_owned()));
/// # }
/// ```
#[proc_macro]
pub fn rhai(input: TokenStream) -> TokenStream {
    let expression = parse_macro_input!(input as syn::LitStr);

    let token = match ENGINE.compile(expression.value()) {
        Ok(_) => quote!({
            use selium_common::types::Executor;
            Executor::Rhai(#expression.to_owned())
        }),
        Err(_) => quote!(std::compile_error!("Invalid Rhai expression")),
    };

    token.into()
}
