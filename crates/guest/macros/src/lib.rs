//! Procedural macros for Selium guest entrypoints and pattern metadata.

#![allow(
    clippy::collapsible_if,
    clippy::indexing_slicing,
    clippy::map_err_ignore,
    clippy::panic,
    clippy::type_complexity,
    clippy::unreachable,
    clippy::unwrap_in_result,
    clippy::unwrap_used,
    reason = "proc-macro helpers intentionally panic on malformed input"
)]

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{
    FnArg, GenericArgument, ItemFn, ItemTrait, PathArguments, ReturnType, Type, parse_macro_input,
};

mod schema;

/// Whether the entrypoint returns `()` or `Result<()>`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EntrypointReturn {
    Unit,
    ResultUnit,
}

/// A classified entrypoint parameter.
#[derive(Clone)]
enum EntrypointParam {
    /// The leading discovery `Context` (consumes the discovery-handle slot).
    Context,
    /// An integer parameter narrowed from a single `i64` slot.
    Integer(Box<Type>),
    /// A `(u64, u64)` pointer parameter (consumes two `i64` slots).
    Pointer,
}

impl EntrypointParam {
    fn slots(&self) -> usize {
        match self {
            EntrypointParam::Context | EntrypointParam::Integer(_) => 1,
            EntrypointParam::Pointer => 2,
        }
    }
}

/// Marks a function as an exported Selium guest entrypoint.
///
/// Accepts entrypoints returning `()` or `Result<()>` whose parameters are an
/// optional leading `Context`, followed by integer parameters
/// (`u8`/`u16`/`u32`/`u64`/`usize` and signed equivalents) and `(u64, u64)`
/// pointer parameters, in sync and async variants.
///
/// One guest module may carry several entrypoints (each function emits its own
/// unique export), but exactly one must be the **poll owner**: the module-global
/// `__selium_guest_poll` reactor-drive export is emitted only by the entrypoint
/// without the `no_poll` flag. Mark every additional entrypoint
/// `#[entrypoint(no_poll)]`.
#[proc_macro_attribute]
pub fn entrypoint(attr: TokenStream, item: TokenStream) -> TokenStream {
    let flags = parse_macro_input!(
        attr with syn::punctuated::Punctuated::<syn::Ident, syn::Token![,]>::parse_terminated
    );
    let no_poll = flags.iter().any(|ident| ident == "no_poll");

    let function = parse_macro_input!(item as ItemFn);
    if !function.sig.generics.params.is_empty() {
        return syn::Error::new_spanned(
            &function.sig.generics,
            "#[entrypoint] does not support generic parameters",
        )
        .to_compile_error()
        .into();
    }

    let Some(return_kind) = inspect_return_type(&function.sig.output) else {
        return syn::Error::new_spanned(
            &function.sig.output,
            "#[entrypoint] return type must be `()` or `Result<()>`",
        )
        .to_compile_error()
        .into();
    };

    let ident = function.sig.ident.clone();
    let metadata_fn = format_ident!("{}_entrypoint_metadata", ident);
    let export_ident = format_ident!("__selium_guest_entrypoint_{}", ident);
    let export_name = ident.to_string();
    let is_async = function.sig.asyncness.is_some();

    // Classify every parameter, rejecting unsupported types up front.
    let mut params = Vec::with_capacity(function.sig.inputs.len());
    for (index, arg) in function.sig.inputs.iter().enumerate() {
        match classify_param(arg, index) {
            Ok(kind) => params.push(kind),
            Err(error) => return error.to_compile_error().into(),
        }
    }

    let generated = generate_entrypoint(
        &function,
        return_kind,
        is_async,
        &ident,
        &export_name,
        &export_ident,
        &params,
    );

    let poll_export = if no_poll {
        quote! {}
    } else {
        quote! {
            /// The host-driven reactor poll export. Only meaningful in the wasm
            /// guest module (`cdylib`) build, where the runtime calls this
            /// export by name to drive the reactor between entrypoint calls.
            ///
            /// Guest crates also compile as native `rlib`s (dev-dependencies
            /// exposing constants and grant metadata to host-side tests), where
            /// this export is dead weight: every `#[entrypoint]` crate emits the
            /// same unmangled symbol, so linking two of them into one native
            /// binary would collide. Gating on wasm keeps exactly one export per
            /// guest module and none in native builds.
            ///
            /// Returns the reactor's completion code: `0` while the poll-owner
            /// entrypoint future is still running (parked), `1` once it has
            /// completed — the host's signal to tear the process down as a
            /// normal exit.
            #[cfg(target_family = "wasm")]
            #[unsafe(export_name = "__selium_guest_poll")]
            pub extern "C" fn __selium_guest_poll() -> i32 {
                ::selium_guest::poll_safely()
            }
        }
    };

    quote! {
        #generated

        #poll_export

        pub fn #metadata_fn() -> ::selium_guest::EntrypointMetadata {
            ::selium_guest::EntrypointMetadata::new(stringify!(#ident))
        }
    }
    .into()
}

/// Generates Selium pattern metadata for a trait interface.
#[proc_macro_attribute]
pub fn pattern_interface(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let interface = parse_macro_input!(item as ItemTrait);
    let ident = interface.ident.clone();
    let metadata_fn = format_ident!("{}_pattern_metadata", ident.to_string().to_lowercase());
    let methods = interface
        .items
        .iter()
        .filter_map(|item| match item {
            syn::TraitItem::Fn(method) => Some(method.sig.ident.to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();

    quote! {
        #interface

        pub fn #metadata_fn() -> ::selium_guest::InterfaceMetadata {
            ::selium_guest::InterfaceMetadata::new(
                stringify!(#ident).to_string(),
                vec![#(::std::string::String::from(#methods)),*],
            )
        }
    }
    .into()
}

/// Struct-level schema annotation declaring a message type.
#[proc_macro_attribute]
pub fn schema(attr: TokenStream, item: TokenStream) -> TokenStream {
    schema::expand(attr, item)
}

/// Classifies one entrypoint parameter by its syntactic shape.
fn classify_param(arg: &FnArg, index: usize) -> syn::Result<EntrypointParam> {
    let FnArg::Typed(pat_type) = arg else {
        return Err(syn::Error::new_spanned(
            arg,
            "#[entrypoint] parameters must be typed",
        ));
    };
    let ty = &*pat_type.ty;

    if is_u64_pair(ty) {
        return Ok(EntrypointParam::Pointer);
    }

    let Type::Path(type_path) = ty else {
        return Err(unsupported_param_error(ty));
    };

    let name = type_path
        .path
        .segments
        .last()
        .map(|segment| segment.ident.to_string())
        .unwrap_or_default();

    match name.as_str() {
        "Context" => {
            if index != 0 {
                return Err(syn::Error::new_spanned(
                    ty,
                    "`Context` must be the first entrypoint parameter",
                ));
            }
            Ok(EntrypointParam::Context)
        }
        name if is_integer_type_name(name) => Ok(EntrypointParam::Integer(Box::new(ty.clone()))),
        _ => Err(unsupported_param_error(ty)),
    }
}

/// Returns the return type suffix for the extern "C" signature: nothing for
/// `()`, `-> i32` for `Result<()>`.
fn extern_return_type(return_kind: EntrypointReturn) -> proc_macro2::TokenStream {
    match return_kind {
        EntrypointReturn::Unit => quote!(),
        EntrypointReturn::ResultUnit => quote!(-> i32),
    }
}

/// Generates the ABI wrapper for a classified parameter list.
///
/// The export takes exactly the total slot count of `i64` parameters and
/// re-binds them into the user function call (constructing `Context`,
/// narrowing integers, and pairing pointer slots) in declaration order.
fn generate_entrypoint(
    function: &ItemFn,
    return_kind: EntrypointReturn,
    is_async: bool,
    ident: &syn::Ident,
    export_name: &str,
    export_ident: &syn::Ident,
    params: &[EntrypointParam],
) -> proc_macro2::TokenStream {
    let init_call =
        quote! { ::selium_guest::init().expect("failed to install Selium guest runtime"); };

    let total_slots: usize = params.iter().map(EntrypointParam::slots).sum();
    let arg_idents: Vec<syn::Ident> = (0..total_slots)
        .map(|index| format_ident!("arg{index}"))
        .collect();

    let mut prelude = Vec::new();
    let mut call_args = Vec::new();
    let mut slot = 0usize;
    for param in params {
        match param {
            EntrypointParam::Context => {
                let arg = &arg_idents[slot];
                slot += 1;
                // A `Result`-returning entrypoint can fail gracefully on a
                // bad bootstrap handle: propagate the `from_raw` error through
                // `?` (the enclosing async block yields `Result<(), E>` and
                // `GuestError: std::error::Error` converts into any released
                // error type, e.g. `anyhow::Error`). Infallible `()` entrypoints
                // have no error channel, so a failed handle aborts the guest.
                let from_raw = match return_kind {
                    EntrypointReturn::ResultUnit => quote! {
                        let ctx = ::selium_guest::Context::from_raw(#arg as u64).await?;
                    },
                    EntrypointReturn::Unit => quote! {
                        let ctx = ::selium_guest::Context::from_raw(#arg as u64)
                            .await
                            .expect("failed to construct bootstrap context");
                    },
                };
                prelude.push(from_raw);
                call_args.push(quote! { ctx });
            }
            EntrypointParam::Integer(ty) => {
                let arg = &arg_idents[slot];
                slot += 1;
                call_args.push(quote! { (#arg as #ty) });
            }
            EntrypointParam::Pointer => {
                let address = &arg_idents[slot];
                let length = &arg_idents[slot + 1];
                slot += 2;
                call_args.push(quote! { (#address as u64, #length as u64) });
            }
        }
    }

    let call_tokens = if is_async {
        quote! { #ident(#(#call_args),*).await }
    } else {
        quote! { #ident(#(#call_args),*) }
    };
    let call = make_call(is_async, return_kind, call_tokens);
    let body = make_wrapper_body(
        return_kind,
        quote! {
            #(#prelude)*
            #call
        },
    );
    let ret = extern_return_type(return_kind);

    quote! {
        #function

        #[unsafe(export_name = #export_name)]
        pub extern "C" fn #export_ident(#(#arg_idents: i64),*) #ret {
            #init_call
            #body
        }
    }
}

/// Inspects the return type to determine whether it is `()`, `Result<()>`, or
/// unsupported. Returns `None` for unsupported types.
fn inspect_return_type(output: &ReturnType) -> Option<EntrypointReturn> {
    match output {
        ReturnType::Default => Some(EntrypointReturn::Unit),
        ReturnType::Type(_, ty) => {
            if is_result_of_unit(ty) {
                Some(EntrypointReturn::ResultUnit)
            } else {
                None
            }
        }
    }
}

fn is_integer_type_name(name: &str) -> bool {
    matches!(
        name,
        "u8" | "u16" | "u32" | "u64" | "usize" | "i8" | "i16" | "i32" | "i64" | "isize"
    )
}

/// Returns true if `ty` is `Result<()>` or `Result<(), E>` (accepts both the
/// two-argument form and single-argument aliases such as `anyhow::Result<()>`
/// whose error type has a default).
fn is_result_of_unit(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty {
        let segments = &type_path.path.segments;
        if let Some(last) = segments.last() {
            if last.ident != "Result" {
                return false;
            }
            if let PathArguments::AngleBracketed(args) = &last.arguments {
                // `Result<(), E>` (two args) or `Result<()>` (alias with default error)
                if matches!(args.args.len(), 1 | 2) {
                    if let Some(GenericArgument::Type(inner_ty)) = args.args.first() {
                        return is_unit_tuple(inner_ty);
                    }
                }
            }
        }
    }
    false
}

fn is_u64_pair(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Tuple(tuple)
            if tuple.elems.len() == 2
                && tuple.elems.iter().all(|element| matches!(
                    element,
                    Type::Path(path) if path.path.is_ident("u64")
                ))
    )
}

/// Returns true if `ty` is the unit type `()`.
fn is_unit_tuple(ty: &Type) -> bool {
    matches!(ty, Type::Tuple(tuple) if tuple.elems.is_empty())
}

/// Generates the call expression for the user function, optionally
/// discarding the result (for `()` return) or returning it (for `Result<()>`).
fn make_call(
    _is_async: bool,
    return_kind: EntrypointReturn,
    call_tokens: proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    match return_kind {
        EntrypointReturn::Unit => quote!(#call_tokens;),
        EntrypointReturn::ResultUnit => call_tokens,
    }
}

/// Generates the extern wrapper body that invokes the user function and
/// optionally matches on `Result<()>` to return `i32`.
fn make_wrapper_body(
    return_kind: EntrypointReturn,
    inner_block: proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    match return_kind {
        EntrypointReturn::Unit => {
            quote! {
                ::selium_guest::run_entrypoint_safely(async move {
                    #inner_block
                });
            }
        }
        EntrypointReturn::ResultUnit => {
            quote! {
                // `run_entrypoint_with_result` logs the error itself,
                // whenever the task completes — before or after the reactor
                // first stalls — and returns `Ok(())` for an entrypoint that
                // parked without completing (a long-running service): the
                // export reports exit code 0 and the task keeps running on
                // the reactor, driven by later polls.
                let result = ::selium_guest::run_entrypoint_with_result(async move {
                    #inner_block
                });
                match result {
                    ::core::result::Result::Ok(()) => 0,
                    ::core::result::Result::Err(_) => 1,
                }
            }
        }
    }
}

fn unsupported_param_error(ty: &Type) -> syn::Error {
    syn::Error::new_spanned(
        ty,
        "unsupported entrypoint parameter; expected `Context` (leading), an integer \
         (u8/u16/u32/u64/usize/i8/i16/i32/i64/isize), or a `(u64, u64)` pointer argument",
    )
}
