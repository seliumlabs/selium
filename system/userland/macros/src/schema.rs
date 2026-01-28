use proc_macro::TokenStream;
use quote::quote;
use syn::{
    Expr, ExprLit, ExprMacro, Item, ItemEnum, ItemStruct, Lit, LitStr, Path as SynPath,
    parse::Parser, parse_macro_input, spanned::Spanned,
};

pub fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    let parser = |input: syn::parse::ParseStream| -> syn::Result<(Option<String>, Option<String>, Option<SynPath>)> {
        let mut path_lit: Option<String> = None;
        let mut fqname: Option<String> = None;
        let mut binding_path: Option<SynPath> = None;
        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            input.parse::<syn::Token![=]>()?;
            if key == "binding" {
                if input.peek(syn::LitStr) {
                    let s: LitStr = input.parse()?;
                    binding_path = Some(syn::parse_str(&s.value())?);
                } else {
                    let p: SynPath = input.parse()?;
                    binding_path = Some(p);
                }
            } else if key == "path" || key == "ty" {
                let expr: Expr = input.parse()?;
                let value = parse_string_expr(expr)?;
                if key == "path" { path_lit = Some(value); } else { fqname = Some(value); }
            } else {
                return Err(input.error("unknown key in #[schema]"));
            }
            if input.peek(syn::Token![,]) {
                let _comma: syn::Token![,] = input.parse()?;
            }
        }
        Ok((path_lit, fqname, binding_path))
    };

    let (path_lit, fqname, binding_path) =
        parser.parse(attr).expect("invalid #[schema] attributes");
    let fbs_path = path_lit.expect("#[schema] requires path = \"...\"");
    let fqname = fqname.expect("#[schema] requires ty = \"ns.Type\"");
    let binding_path = binding_path.expect("#[schema] requires binding = path::to::Type");

    let base = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR missing");
    let full = std::path::Path::new(&base).join(&fbs_path);
    let bytes =
        std::fs::read(&full).unwrap_or_else(|e| panic!("cannot read {}: {}", full.display(), e));
    let hash = blake3::hash(&bytes);
    let hash_bytes = &hash.as_bytes()[0..16];
    let hash_lit = syn::LitByteStr::new(hash_bytes, proc_macro2::Span::call_site());

    let item = parse_macro_input!(item as Item);
    match item {
        Item::Struct(st) => expand_struct(st, fqname, binding_path, hash_lit),
        Item::Enum(en) => expand_enum(en, fqname, binding_path, hash_lit),
        other => syn::Error::new_spanned(other, "#[schema] requires a struct or enum")
            .to_compile_error()
            .into(),
    }
}

fn expand_struct(
    st: ItemStruct,
    fqname: String,
    binding_path: SynPath,
    hash_lit: syn::LitByteStr,
) -> TokenStream {
    let mut st2 = st.clone();
    st2.attrs = st
        .attrs
        .iter()
        .filter(|attr| !attr.path().is_ident("schema"))
        .cloned()
        .collect();
    let struct_ident = st.ident.clone();
    let schema_ident = syn::Ident::new(
        &format!("{}Schema", struct_ident),
        proc_macro2::Span::call_site(),
    );
    let binding_ident = binding_path.segments.last().unwrap().ident.clone();
    let args_ident = syn::Ident::new(
        &format!("{}Args", binding_ident),
        proc_macro2::Span::call_site(),
    );

    let mut args_segments = binding_path.segments.clone();
    args_segments.pop();
    args_segments.push(syn::PathSegment {
        ident: args_ident.clone(),
        arguments: syn::PathArguments::None,
    });
    let args_path = syn::Path {
        leading_colon: binding_path.leading_colon,
        segments: args_segments,
    };

    let fields = match &st.fields {
        syn::Fields::Named(named) => named.named.iter().collect::<Vec<_>>(),
        _ => panic!("#[schema] requires a struct with named fields"),
    };

    let ctor_params = fields.iter().map(|f| {
        let id = f.ident.as_ref().unwrap();
        let ty = &f.ty;
        quote! { #id: #ty }
    });
    let ctor_inits = fields.iter().map(|f| {
        let id = f.ident.as_ref().unwrap();
        quote! { #id }
    });

    let encode_fields = fields.iter().map(|f| encode_field(f));
    let decode_fields = fields.iter().map(|f| decode_field(f));

    let fq_lit = fqname.clone();
    let binding_path_ts = quote! { #binding_path };

    let expanded = quote! {
        #st2

        #[allow(non_upper_case_globals)]
        pub const #schema_ident: selium_userland::encoding::SchemaDescriptor = selium_userland::encoding::SchemaDescriptor {
            fqname: #fq_lit,
            hash: *#hash_lit,
        };

        impl selium_userland::encoding::HasSchema for #struct_ident {
            const SCHEMA: selium_userland::encoding::SchemaDescriptor = #schema_ident;
        }

        impl selium_userland::encoding::FieldEncoder for #struct_ident {
            type Output<'bldr> = Option<flatbuffers::WIPOffset<#binding_path_ts<'bldr>>>;

            fn encode_field<'bldr, A: flatbuffers::Allocator + 'bldr>(
                &self,
                builder: &mut flatbuffers::FlatBufferBuilder<'bldr, A>,
            ) -> Self::Output<'bldr> {
                Some(self.write_flatbuffer(builder))
            }
        }

        impl #struct_ident {
            pub fn new( #( #ctor_params ),* ) -> Self {
                Self { #( #ctor_inits, )* }
            }

            pub fn write_flatbuffer<'bldr, A: flatbuffers::Allocator + 'bldr>(
                &self,
                builder: &mut flatbuffers::FlatBufferBuilder<'bldr, A>,
            ) -> flatbuffers::WIPOffset<#binding_path_ts<'bldr>> {
                let mut args = #args_path::default();
                #( #encode_fields )*
                #binding_path_ts::create(builder, &args)
            }

            pub fn from_flatbuffer(view: #binding_path_ts<'_>) -> Self {
                Self { #( #decode_fields, )* }
            }
        }

        impl selium_userland::encoding::FlatMsg for #struct_ident {
            fn encode(value: &Self) -> Vec<u8> {
                let mut builder = flatbuffers::FlatBufferBuilder::new();
                let root = value.write_flatbuffer(&mut builder);
                builder.finish(root, None);
                builder.finished_data().to_vec()
            }

            fn decode(bytes: &[u8]) -> Result<Self, flatbuffers::InvalidFlatbuffer> {
                let view = flatbuffers::root::<#binding_path_ts<'_>>(bytes)?;
                Ok(Self::from_flatbuffer(view))
            }
        }
    };

    expanded.into()
}

fn expand_enum(
    en: ItemEnum,
    fqname: String,
    binding_path: SynPath,
    hash_lit: syn::LitByteStr,
) -> TokenStream {
    let mut en2 = en.clone();
    en2.attrs = en
        .attrs
        .iter()
        .filter(|attr| !attr.path().is_ident("schema"))
        .cloned()
        .collect();
    let enum_ident = en.ident.clone();
    let schema_ident = syn::Ident::new(
        &format!("{}Schema", enum_ident),
        proc_macro2::Span::call_site(),
    );
    let fq_lit = fqname.clone();
    let binding_path_ts = quote! { #binding_path };

    let mut unit_variants = Vec::new();
    let mut fallback_variant: Option<(syn::Ident, syn::Type)> = None;
    for variant in en.variants.iter() {
        match &variant.fields {
            syn::Fields::Unit => unit_variants.push(variant.ident.clone()),
            syn::Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
                if fallback_variant.is_some() {
                    return syn::Error::new_spanned(
                        variant,
                        "#[schema] enums may only include a single tuple variant",
                    )
                    .to_compile_error()
                    .into();
                }
                let Some(field) = fields.unnamed.first() else {
                    return syn::Error::new_spanned(
                        variant,
                        "#[schema] enums require a single tuple field",
                    )
                    .to_compile_error()
                    .into();
                };
                fallback_variant = Some((variant.ident.clone(), field.ty.clone()));
            }
            _ => {
                return syn::Error::new_spanned(
                    variant,
                    "#[schema] enums require unit variants and at most one tuple fallback",
                )
                .to_compile_error()
                .into();
            }
        }
    }

    if unit_variants.is_empty() {
        return syn::Error::new_spanned(en, "#[schema] enums require at least one unit variant")
            .to_compile_error()
            .into();
    }

    let to_flatbuffer_variants = unit_variants.iter().map(|variant| {
        quote! { Self::#variant => #binding_path_ts::#variant, }
    });
    let from_flatbuffer_variants = unit_variants.iter().map(|variant| {
        quote! { #binding_path_ts::#variant => Self::#variant, }
    });

    let fallback_to_flatbuffer = fallback_variant.as_ref().map(|(ident, _)| {
        quote! { Self::#ident(value) => #binding_path_ts(*value), }
    });

    let default_variant = unit_variants.first().cloned();
    let fallback_from_flatbuffer = if let Some((ident, _ty)) = fallback_variant {
        quote! { other => Self::#ident(other.0), }
    } else if let Some(variant) = default_variant {
        quote! { _ => Self::#variant, }
    } else {
        quote! { _ => unreachable!(), }
    };

    let expanded = quote! {
        #en2

        #[allow(non_upper_case_globals)]
        pub const #schema_ident: selium_userland::encoding::SchemaDescriptor = selium_userland::encoding::SchemaDescriptor {
            fqname: #fq_lit,
            hash: *#hash_lit,
        };

        impl selium_userland::encoding::HasSchema for #enum_ident {
            const SCHEMA: selium_userland::encoding::SchemaDescriptor = #schema_ident;
        }

        impl selium_userland::encoding::FieldEncoder for #enum_ident {
            type Output<'bldr> = #binding_path_ts;

            fn encode_field<'bldr, A: flatbuffers::Allocator + 'bldr>(
                &self,
                builder: &mut flatbuffers::FlatBufferBuilder<'bldr, A>,
            ) -> Self::Output<'bldr> {
                self.write_flatbuffer(builder)
            }
        }

        impl #enum_ident {
            pub fn write_flatbuffer<'bldr, A: flatbuffers::Allocator + 'bldr>(
                &self,
                _builder: &mut flatbuffers::FlatBufferBuilder<'bldr, A>,
            ) -> #binding_path_ts {
                match self {
                    #( #to_flatbuffer_variants )*
                    #fallback_to_flatbuffer
                }
            }

            pub fn from_flatbuffer(value: #binding_path_ts) -> Self {
                match value {
                    #( #from_flatbuffer_variants )*
                    #fallback_from_flatbuffer
                }
            }
        }
    };

    expanded.into()
}

fn encode_field(field: &syn::Field) -> proc_macro2::TokenStream {
    let id = field.ident.as_ref().unwrap();
    match &field.ty {
        syn::Type::Path(tp) => {
            if let Some(inner) = option_inner(tp) {
                encode_option_field(id, inner)
            } else if let Some(inner) = vec_inner(tp) {
                encode_vec_field(id, inner, true)
            } else {
                let ident = &tp.path.segments.last().unwrap().ident;
                if ident == "String" {
                    quote! { args.#id = Some(builder.create_string(&self.#id)); }
                } else if is_scalar_ident(ident) {
                    quote! { args.#id = self.#id; }
                } else {
                    quote! {
                        args.#id = selium_userland::encoding::FieldEncoder::encode_field(
                            &self.#id,
                            builder,
                        );
                    }
                }
            }
        }
        _ => quote! { args.#id = self.#id; },
    }
}

fn decode_field(field: &syn::Field) -> proc_macro2::TokenStream {
    let id = field.ident.as_ref().unwrap();
    match &field.ty {
        syn::Type::Path(tp) => {
            if let Some(inner) = option_inner(tp) {
                decode_option_field(id, inner)
            } else if let Some(inner) = vec_inner(tp) {
                decode_vec_field(id, inner, true)
            } else {
                let ident = &tp.path.segments.last().unwrap().ident;
                if ident == "String" {
                    quote! { #id: selium_userland::encoding::StringFieldValue::into_owned(view.#id()) }
                } else if is_scalar_ident(ident) {
                    quote! { #id: view.#id() }
                } else {
                    quote! { #id: #tp::from_flatbuffer(view.#id()) }
                }
            }
        }
        _ => quote! { #id: view.#id() },
    }
}

fn encode_option_field(id: &syn::Ident, inner: &syn::Type) -> proc_macro2::TokenStream {
    if let syn::Type::Path(tp) = inner {
        if let Some(vec_inner) = vec_inner(tp) {
            return encode_vec_field(id, vec_inner, false);
        }
        let ident = &tp.path.segments.last().unwrap().ident;
        if ident == "String" {
            return quote! {
                args.#id = self.#id.as_ref().map(|value| builder.create_string(value));
            };
        }
        if is_scalar_ident(ident) {
            return quote! { args.#id = self.#id; };
        }
        return quote! {
            args.#id = self.#id.as_ref().map(|value| value.write_flatbuffer(builder));
        };
    }

    quote! { args.#id = self.#id.as_ref().cloned(); }
}

fn decode_option_field(id: &syn::Ident, inner: &syn::Type) -> proc_macro2::TokenStream {
    if let syn::Type::Path(tp) = inner {
        if let Some(vec_inner) = vec_inner(tp) {
            return decode_vec_field(id, vec_inner, false);
        }
        let ident = &tp.path.segments.last().unwrap().ident;
        if ident == "String" {
            return quote! { #id: view.#id().map(|value| value.to_string()) };
        }
        if is_scalar_ident(ident) {
            return quote! { #id: view.#id() };
        }
        return quote! { #id: view.#id().map(#tp::from_flatbuffer) };
    }

    quote! { #id: view.#id() }
}

fn encode_vec_field(
    id: &syn::Ident,
    inner: &syn::Type,
    is_required: bool,
) -> proc_macro2::TokenStream {
    if let syn::Type::Path(tp) = inner {
        let ident = &tp.path.segments.last().unwrap().ident;
        if ident == "u8" {
            if is_required {
                return quote! { args.#id = Some(builder.create_vector(&self.#id)); };
            }
            return quote! {
                args.#id = self.#id.as_ref().map(|value| builder.create_vector(value));
            };
        }
        if ident == "String" {
            let offsets_ident =
                syn::Ident::new(&format!("{}_offsets", id), proc_macro2::Span::call_site());
            if is_required {
                return quote! {
                    let #offsets_ident: Vec<_> = self.#id.iter().map(|value| builder.create_string(value)).collect();
                    args.#id = Some(builder.create_vector(&#offsets_ident));
                };
            }
            return quote! {
                args.#id = self.#id.as_ref().map(|value| {
                    let #offsets_ident: Vec<_> = value.iter().map(|item| builder.create_string(item)).collect();
                    builder.create_vector(&#offsets_ident)
                });
            };
        }
        if is_scalar_ident(ident) {
            if is_required {
                return quote! { args.#id = Some(builder.create_vector(&self.#id)); };
            }
            return quote! {
                args.#id = self.#id.as_ref().map(|value| builder.create_vector(value));
            };
        }
        let offsets_ident =
            syn::Ident::new(&format!("{}_offsets", id), proc_macro2::Span::call_site());
        if is_required {
            return quote! {
                let #offsets_ident: Vec<_> = self.#id
                    .iter()
                    .map(|item| item.write_flatbuffer(builder))
                    .collect();
                args.#id = Some(builder.create_vector(&#offsets_ident));
            };
        }
        return quote! {
            args.#id = self.#id.as_ref().map(|value| {
                let #offsets_ident: Vec<_> = value
                    .iter()
                    .map(|item| item.write_flatbuffer(builder))
                    .collect();
                builder.create_vector(&#offsets_ident)
            });
        };
    }

    quote! { args.#id = self.#id.as_ref().map(|value| value.write_flatbuffer(builder)); }
}

fn decode_vec_field(
    id: &syn::Ident,
    inner: &syn::Type,
    is_required: bool,
) -> proc_macro2::TokenStream {
    if let syn::Type::Path(tp) = inner {
        let ident = &tp.path.segments.last().unwrap().ident;
        if ident == "u8" {
            if is_required {
                return quote! {
                    #id: view.#id().map(|value| value.bytes().to_vec()).unwrap_or_default()
                };
            }
            return quote! {
                #id: view.#id().map(|value| value.bytes().to_vec())
            };
        }
        if ident == "String" {
            let map_expr = quote! { value.iter().map(|item| item.to_string()).collect::<::std::vec::Vec<_>>() };
            if is_required {
                return quote! { #id: view.#id().map(|value| #map_expr).unwrap_or_default() };
            }
            return quote! { #id: view.#id().map(|value| #map_expr) };
        }
        if is_scalar_ident(ident) {
            if is_required {
                return quote! {
                    #id: view.#id().map(|value| value.iter().collect::<::std::vec::Vec<_>>()).unwrap_or_default()
                };
            }
            return quote! { #id: view.#id().map(|value| value.iter().collect::<::std::vec::Vec<_>>()) };
        }
    }

    let map_expr =
        quote! { value.iter().map(#inner::from_flatbuffer).collect::<::std::vec::Vec<_>>() };
    if is_required {
        return quote! { #id: view.#id().map(|value| #map_expr).unwrap_or_default() };
    }

    quote! { #id: view.#id().map(|value| #map_expr) }
}

fn option_inner(tp: &syn::TypePath) -> Option<&syn::Type> {
    let last = tp.path.segments.last().unwrap();
    if last.ident != "Option" {
        return None;
    }

    if let syn::PathArguments::AngleBracketed(args) = &last.arguments
        && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
    {
        return Some(inner);
    }

    None
}

fn vec_inner(tp: &syn::TypePath) -> Option<&syn::Type> {
    let last = tp.path.segments.last().unwrap();
    if last.ident != "Vec" {
        return None;
    }

    if let syn::PathArguments::AngleBracketed(args) = &last.arguments
        && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
    {
        return Some(inner);
    }

    None
}

fn is_scalar_ident(ident: &proc_macro2::Ident) -> bool {
    matches!(
        ident.to_string().as_str(),
        "bool"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "usize"
            | "isize"
            | "f32"
            | "f64"
    )
}

fn parse_string_expr(expr: Expr) -> syn::Result<String> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Str(lit), ..
        }) => Ok(lit.value()),
        Expr::Macro(ExprMacro { mac, .. }) if mac.path.is_ident("concat") => {
            parse_concat_macro(mac)
        }
        Expr::Macro(ExprMacro { mac, .. }) if mac.path.is_ident("env") => parse_env_macro(mac),
        other => Err(syn::Error::new_spanned(other, "expected string literal")),
    }
}

fn parse_concat_macro(mac: syn::Macro) -> syn::Result<String> {
    let tokens = syn::parse::Parser::parse2(
        syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated,
        mac.tokens,
    )?;
    let mut out = String::new();
    for expr in tokens {
        out.push_str(&parse_string_expr(expr)?);
    }
    Ok(out)
}

fn parse_env_macro(mac: syn::Macro) -> syn::Result<String> {
    let span = mac.path.span();
    let args = syn::parse::Parser::parse2(
        syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated,
        mac.tokens.clone(),
    )?;
    let first = args
        .first()
        .ok_or_else(|| syn::Error::new(span, "env! requires an argument"))?;
    if let Expr::Lit(ExprLit {
        lit: Lit::Str(lit), ..
    }) = first
    {
        let var = lit.value();
        std::env::var(&var).map_err(|_| {
            syn::Error::new_spanned(lit, format!("environment variable {var} not set"))
        })
    } else {
        Err(syn::Error::new_spanned(
            first,
            "env! argument must be a string literal",
        ))
    }
}
