use proc_macro::TokenStream;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::Span;
use quote::quote;
use syn::{
    Error, FnArg, Ident, ItemFn, Pat, PatIdent, PatType, ReturnType, Type, parse_macro_input,
    parse_quote,
};

enum RetKind {
    Unit,
    Result,
}

struct ParamSpec {
    ident: Ident,
    mutable: bool,
    ty: Type,
    kind: ParamKind,
    original: PatType,
}

enum ParamKind {
    Direct,
    Decode {
        ptr_ident: Ident,
        len_ident: Ident,
    },
    SplitInt {
        low_ident: Ident,
        high_ident: Ident,
        signed: bool,
    },
    Context {
        holder_ident: Ident,
        by_ref: bool,
    },
}

enum ContextMode {
    Owned,
    Ref,
    MutRef,
}

pub fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return Error::new(
            proc_macro2::Span::call_site(),
            "#[entrypoint] does not take arguments",
        )
        .to_compile_error()
        .into();
    }

    let f = parse_macro_input!(item as ItemFn);

    if !f.sig.generics.params.is_empty() {
        return Error::new_spanned(&f.sig.generics, "#[entrypoint] does not support generics")
            .to_compile_error()
            .into();
    }

    let params = match validate_params(&f) {
        Ok(idents) => idents,
        Err(err) => return err.to_compile_error().into(),
    };

    let ret_kind = match classify_return(&f.sig.output) {
        Ok(kind) => kind,
        Err(err) => return err.to_compile_error().into(),
    };

    let orig_ident = f.sig.ident.clone();
    let vis = f.vis.clone();
    let attrs: Vec<_> = f
        .attrs
        .iter()
        .filter(|attr| !attr.path().is_ident("entrypoint"))
        .cloned()
        .collect();
    let user_ident = Ident::new(&format!("__selium_user_{}", orig_ident), Span::call_site());

    let (log_uri_inputs, log_uri_binding, init_logging) = {
        let log_uri_ptr = Ident::new("__selium_log_uri_ptr", Span::call_site());
        let log_uri_len = Ident::new("__selium_log_uri_len", Span::call_site());
        let log_uri_opt = Ident::new("__selium_log_uri_opt", Span::call_site());
        let inputs = vec![
            parse_quote! { #log_uri_ptr: *const u8 },
            parse_quote! { #log_uri_len: u32 },
        ];
        let binding = quote! {
            let #log_uri_opt: Option<&str> = if #log_uri_len == 0 {
                None
            } else {
                if #log_uri_ptr.is_null() {
                    panic!("entrypoint log URI provided a null pointer with non-zero length");
                }
                let len = match usize::try_from(#log_uri_len) {
                    Ok(len) => len,
                    Err(_) => panic!("entrypoint log URI length does not fit usize"),
                };
                let bytes: &[u8] = unsafe { core::slice::from_raw_parts(#log_uri_ptr, len) };
                Some(core::str::from_utf8(bytes).unwrap_or_else(|err| {
                    panic!("failed to decode entrypoint log URI: {}", err);
                }))
            };
        };
        let init = quote! { selium_userland::logging::init_with_log_uri(#log_uri_opt) };
        (inputs, binding, init)
    };

    let atlas_crate = match crate_name("selium-atlas") {
        Ok(FoundCrate::Name(name)) => {
            let ident = Ident::new(&name, Span::call_site());
            Some(quote!(#ident))
        }
        Ok(FoundCrate::Itself) => Some(quote!(crate)),
        Err(_) => None,
    };

    let install_log_uri_registrar = atlas_crate
        .map(|atlas_crate| {
            quote! {
                if !selium_userland::logging::log_uri_registrar_installed() {
                    let atlas_available = selium_userland::block_on(async {
                        match selium_userland::singleton::lookup(
                            <#atlas_crate::Atlas as selium_userland::Dependency>::DESCRIPTOR.id,
                        )
                        .await
                        {
                            Ok(_handle) => true,
                            Err(_err) => false,
                        }
                    });

                    if atlas_available {
                        if let Err(err) = #atlas_crate::install_log_uri_registrar() {
                            panic!("failed to install atlas log URI registrar: {}", err);
                        }
                    }
                }
            }
        })
        .unwrap_or_else(|| quote! {});

    let mut user_sig = f.sig.clone();
    user_sig.ident = user_ident.clone();
    let user_block = f.block.clone();
    let arg_idents: Vec<_> = params.iter().map(|p| p.ident.clone()).collect();

    let call_user = if f.sig.asyncness.is_some() {
        quote! { selium_userland::block_on(#user_ident(#(#arg_idents),*)) }
    } else {
        quote! { #user_ident(#(#arg_idents),*) }
    };

    let run_user = match ret_kind {
        RetKind::Unit => quote! {
            #call_user;
        },
        RetKind::Result => quote! {
            if let Err(err) = #call_user {
                panic!("entrypoint {} failed: {:?}", stringify!(#orig_ident), err);
            }
        },
    };

    let user_fn = quote! {
        #(#attrs)*
        #vis #user_sig #user_block
    };

    let mut entrypoint_inputs = Vec::new();
    entrypoint_inputs.extend(log_uri_inputs);
    entrypoint_inputs.extend(params.iter().flat_map(|param| match &param.kind {
        ParamKind::Direct => vec![FnArg::Typed(param.original.clone())],
        ParamKind::Decode {
            ptr_ident,
            len_ident,
        } => vec![
            parse_quote! { #ptr_ident: *const u8 },
            parse_quote! { #len_ident: u32 },
        ],
        ParamKind::SplitInt {
            low_ident,
            high_ident,
            ..
        } => vec![
            parse_quote! { #low_ident: selium_userland::abi::GuestInt },
            parse_quote! { #high_ident: selium_userland::abi::GuestInt },
        ],
        ParamKind::Context { .. } => Vec::new(),
    }));

    let decode_bindings: Vec<_> = params
        .iter()
        .filter_map(|param| match &param.kind {
            ParamKind::Direct => None,
            ParamKind::Decode { ptr_ident, len_ident } => {
                let ident = &param.ident;
                let ty = &param.ty;
                let mutability = if param.mutable {
                    quote! { mut }
                } else {
                    quote! {}
                };

                let decode_value = if is_str_type(ty) {
                    quote! {
                        if #len_ident == 0 {
                            ""
                        } else {
                            if #ptr_ident.is_null() {
                                panic!(
                                    "entrypoint argument {} provided a null pointer with non-zero length",
                                    stringify!(#ident),
                                );
                            }
                            let len = match usize::try_from(#len_ident) {
                                Ok(len) => len,
                                Err(_) => panic!(
                                    "entrypoint argument {} length does not fit usize",
                                    stringify!(#ident),
                                ),
                            };
                            let bytes: &[u8] = unsafe { core::slice::from_raw_parts(#ptr_ident, len) };

                            core::str::from_utf8(bytes).unwrap_or_else(|err| {
                                panic!(
                                    "failed to decode entrypoint argument {}: {}",
                                    stringify!(#ident),
                                    err,
                                )
                            })
                        }
                    }
                } else {
                    quote! {
                        let len = match usize::try_from(#len_ident) {
                            Ok(len) => len,
                            Err(_) => panic!(
                                "entrypoint argument {} length does not fit usize",
                                stringify!(#ident),
                            ),
                        };
                        let bytes: &[u8] = if len == 0 {
                            &[]
                        } else {
                            if #ptr_ident.is_null() {
                                panic!(
                                    "entrypoint argument {} provided a null pointer with non-zero length",
                                    stringify!(#ident),
                                );
                            }
                            unsafe { core::slice::from_raw_parts(#ptr_ident, len) }
                        };

                        match selium_userland::abi::decode_rkyv::<#ty>(bytes) {
                            Ok(decoded) => decoded,
                            Err(err) => panic!(
                                "failed to decode entrypoint argument {}: {}",
                                stringify!(#ident),
                                err,
                            ),
                        }
                    }
                };

                Some(quote! {
                    let #mutability #ident: #ty = {
                        #decode_value
                    };
                })
            }
            ParamKind::Context {
                holder_ident,
                by_ref,
            } => {
                let ident = &param.ident;
                let ty = &param.ty;
                let mutability = if param.mutable {
                    quote! { mut }
                } else {
                    quote! {}
                };
                if *by_ref {
                    Some(quote! {
                        let #holder_ident = selium_userland::Context::current();
                        let #mutability #ident: #ty = &#holder_ident;
                    })
                } else {
                    Some(quote! {
                        let #mutability #ident: #ty = selium_userland::Context::current();
                    })
                }
            }
            ParamKind::SplitInt {
                low_ident,
                high_ident,
                signed,
            } => {
                let ident = &param.ident;
                let ty = &param.ty;
                Some(quote! {
                    let #ident: #ty = {
                        let lo_bits = u32::from_ne_bytes(#low_ident.to_ne_bytes());
                        let hi_bits = u32::from_ne_bytes(#high_ident.to_ne_bytes());
                        let combined = (u64::from(hi_bits) << 32) | u64::from(lo_bits);
                        if #signed {
                            let signed = i64::from_le_bytes(combined.to_le_bytes());
                            signed as #ty
                        } else {
                            combined as #ty
                        }
                    };
                })
            }
        })
        .collect();

    let entrypoint = quote! {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn #orig_ident(#(#entrypoint_inputs),*) {
            #log_uri_binding
            #install_log_uri_registrar
            if let Err(err) = #init_logging {
                panic!("failed to initialise logging bridge: {}", err);
            }
            #(#decode_bindings)*
            #run_user
        }
    };

    let tokens = quote! {
        #user_fn
        #entrypoint
    };

    tokens.into()
}

fn classify_return(ret: &ReturnType) -> Result<RetKind, Error> {
    match ret {
        ReturnType::Default => Ok(RetKind::Unit),
        ReturnType::Type(_, ty) => match ty.as_ref() {
            Type::Tuple(tuple) if tuple.elems.is_empty() => Ok(RetKind::Unit),
            Type::Path(path) if is_result_unit(path) => Ok(RetKind::Result),
            other => Err(Error::new_spanned(
                other,
                "#[entrypoint] functions must return () or Result<(), E>",
            )),
        },
    }
}

fn validate_params(f: &ItemFn) -> Result<Vec<ParamSpec>, Error> {
    let mut params = Vec::new();
    let mut context_seen = None;

    for input in &f.sig.inputs {
        let (pat, ty) = match input {
            FnArg::Receiver(_) => {
                return Err(Error::new_spanned(
                    input,
                    "#[entrypoint] functions cannot take a receiver argument",
                ));
            }
            FnArg::Typed(pat_type) => (&*pat_type.pat, &*pat_type.ty),
        };

        let Pat::Ident(PatIdent {
            ident, mutability, ..
        }) = pat
        else {
            return Err(Error::new_spanned(
                pat,
                "#[entrypoint] parameters must use identifier patterns",
            ));
        };

        let ident = ident.clone();
        let ty = ty.clone();
        let kind = if let Some(mode) = context_mode(&ty) {
            if context_seen.is_some() {
                return Err(Error::new_spanned(
                    &ty,
                    "#[entrypoint] supports at most one Context parameter",
                ));
            }
            if matches!(mode, ContextMode::MutRef) {
                return Err(Error::new_spanned(
                    &ty,
                    "#[entrypoint] Context parameters must be passed by value or shared reference",
                ));
            }
            context_seen = Some(());
            ParamKind::Context {
                holder_ident: Ident::new(&format!("__selium_ctx_{}", ident), Span::call_site()),
                by_ref: matches!(mode, ContextMode::Ref),
            }
        } else {
            classify_param_kind(&ident, &ty)
        };

        params.push(ParamSpec {
            ident,
            mutable: mutability.is_some(),
            ty,
            kind,
            original: match input {
                FnArg::Typed(pat_type) => pat_type.clone(),
                FnArg::Receiver(_) => unreachable!(),
            },
        });
    }

    Ok(params)
}

fn classify_param_kind(ident: &Ident, ty: &Type) -> ParamKind {
    if is_split_int(ty) {
        ParamKind::SplitInt {
            low_ident: Ident::new(&format!("__selium_raw_{}_lo", ident), Span::call_site()),
            high_ident: Ident::new(&format!("__selium_raw_{}_hi", ident), Span::call_site()),
            signed: matches!(ty, Type::Path(path) if path.path.is_ident("i64")),
        }
    } else if is_direct_type(ty) {
        ParamKind::Direct
    } else {
        ParamKind::Decode {
            ptr_ident: Ident::new(&format!("__selium_raw_{}_ptr", ident), Span::call_site()),
            len_ident: Ident::new(&format!("__selium_raw_{}_len", ident), Span::call_site()),
        }
    }
}

fn context_mode(ty: &Type) -> Option<ContextMode> {
    match ty {
        Type::Path(path) if is_context_path(&path.path) => Some(ContextMode::Owned),
        Type::Reference(reference) => {
            let Type::Path(path) = reference.elem.as_ref() else {
                return None;
            };
            if !is_context_path(&path.path) {
                return None;
            }
            if reference.mutability.is_some() {
                Some(ContextMode::MutRef)
            } else {
                Some(ContextMode::Ref)
            }
        }
        _ => None,
    }
}

fn is_context_path(path: &syn::Path) -> bool {
    let Some(seg) = path.segments.last() else {
        return false;
    };

    seg.ident == "Context" && seg.arguments.is_empty()
}

fn is_direct_type(ty: &Type) -> bool {
    matches!(ty, Type::Ptr(_)) || (is_scalar_type(ty) && !is_split_int(ty))
}

fn is_scalar_type(ty: &Type) -> bool {
    let Type::Path(path) = ty else { return false };
    let Some(seg) = path.path.segments.last() else {
        return false;
    };

    if !seg.arguments.is_empty() {
        return false;
    }

    matches!(
        seg.ident.to_string().as_str(),
        "i8" | "u8"
            | "i16"
            | "u16"
            | "i32"
            | "u32"
            | "isize"
            | "usize"
            | "i64"
            | "u64"
            | "f32"
            | "f64"
            | "GuestUint"
            | "GuestInt"
            | "GuestResourceId"
    )
}

fn is_split_int(ty: &Type) -> bool {
    let Type::Path(path) = ty else { return false };
    let Some(seg) = path.path.segments.last() else {
        return false;
    };

    matches!(
        seg.ident.to_string().as_str(),
        "i64" | "u64" | "GuestResourceId"
    )
}

fn is_str_type(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Reference(reference)
            if matches!(&*reference.elem, Type::Path(path) if path.path.is_ident("str"))
    )
}

fn is_result_unit(path: &syn::TypePath) -> bool {
    if let Some(seg) = path.path.segments.last()
        && seg.ident == "Result"
        && let syn::PathArguments::AngleBracketed(args) = &seg.arguments
        && let Some(syn::GenericArgument::Type(Type::Tuple(tuple))) = args.args.first()
    {
        return tuple.elems.is_empty();
    }

    false
}
