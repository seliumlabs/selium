use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{
    Expr, ExprLit, ExprMacro, Item, ItemEnum, ItemStruct, Lit, LitStr, Path as SynPath,
    parse::Parser, parse_macro_input, spanned::Spanned,
};

#[derive(Clone)]
enum FieldKind {
    /// `field: K` — direct generic type parameter.
    Generic,
    /// `field: Option<V>` where `V` is a generic type parameter.
    OptionGeneric,
    /// `field: Option<u64>` — optional scalar (sentinel encoding).
    OptionScalar,
    /// Concrete non-generic type — pass through unchanged.
    Concrete,
}

/// Classified field shape of a data-carrying enum variant.
enum DataFieldKind {
    /// `String` table field.
    String,
    /// Primitive numeric/bool table field.
    Scalar,
    /// `Vec<u8>` table field.
    Bytes,
    /// `Option<String>` table field.
    OptionString,
    /// `Option<scalar>` table field encoded with a zero sentinel.
    OptionScalar,
    /// Nested `#[schema]` message field (required or optional).
    Nested { optional: bool },
    /// `Vec<T>` of nested `#[schema]` messages.
    NestedVec,
    /// `#[schema(skip)]`: omitted on encode, defaulted on decode.
    Skip,
}

/// One declared field inside a data-carrying variant.
struct DataField {
    /// The Rust field name; `None` for a single unnamed-field (tuple) variant.
    ident: Option<syn::Ident>,
    /// The FlatBuffers table field name it maps to.
    wire_name: String,
    /// The field's declared Rust type.
    ty: syn::Type,
    kind: DataFieldKind,
}

/// The declared shape of a data-carrying variant.
enum DataVariantShape {
    Unit,
    Named(Vec<DataField>),
    Tuple(Box<DataField>),
}

/// One processed data-carrying variant.
struct DataVariant {
    ident: syn::Ident,
    tag: u8,
    shape: DataVariantShape,
}

pub fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    let parser = |input: syn::parse::ParseStream| -> syn::Result<(
        Option<String>,
        Option<String>,
        Option<SynPath>,
        Option<syn::Ident>,
    )> {
        let mut path_lit: Option<String> = None;
        let mut fqname: Option<String> = None;
        let mut binding_path: Option<SynPath> = None;
        let mut wire_ident: Option<syn::Ident> = None;
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
            } else if key == "wire" {
                if input.peek(syn::LitStr) {
                    let s: LitStr = input.parse()?;
                    wire_ident = Some(syn::Ident::new(&s.value(), s.span()));
                } else {
                    wire_ident = Some(input.parse()?);
                }
            } else if key == "path" || key == "ty" {
                let expr: Expr = input.parse()?;
                let value = parse_string_expr(expr)?;
                if key == "path" {
                    path_lit = Some(value);
                } else {
                    fqname = Some(value);
                }
            } else {
                return Err(input.error("unknown key in #[schema]"));
            }
            if input.peek(syn::Token![,]) {
                let _comma: syn::Token![,] = input.parse()?;
            }
        }
        Ok((path_lit, fqname, binding_path, wire_ident))
    };

    let (path_lit, fqname, binding_path, wire_ident) =
        parser.parse(attr).expect("invalid #[schema] attributes");
    let fbs_path = path_lit.expect("#[schema] requires path = \"...\"");
    let fqname = fqname.expect("#[schema] requires ty = \"ns.Type\"");
    let binding_path = binding_path.expect("#[schema] requires binding = path::to::Type");

    let item = parse_macro_input!(item as Item);

    // Early check: generic structs require the wire parameter.
    // This runs before .fbs file I/O so the error is clean.
    if let Item::Struct(ref st) = item {
        let generic_params = collect_generic_params(&st.generics);
        if !generic_params.is_empty() && wire_ident.is_none() {
            return syn::Error::new_spanned(
                &st.generics,
                "struct has generic type parameters; add wire = WireTypeName to #[schema] to generate a wire type",
            )
            .to_compile_error()
            .into();
        }
    }

    let base = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR missing");
    let full = std::path::Path::new(&base).join(&fbs_path);
    let bytes =
        std::fs::read(&full).unwrap_or_else(|e| panic!("cannot read {}: {}", full.display(), e));
    let hash = blake3::hash(&bytes);
    let hash_bytes = &hash.as_bytes()[0..16];
    let hash_lit = syn::LitByteStr::new(hash_bytes, proc_macro2::Span::call_site());

    match item {
        Item::Struct(st) => expand_struct(st, fbs_path, fqname, binding_path, hash_lit, wire_ident),
        Item::Enum(en) => {
            if data_enum_has_fields(&en) {
                expand_data_enum(en, fqname, binding_path, hash_lit)
                    .unwrap_or_else(|error| error.into_compile_error().into())
            } else {
                expand_enum(en, fqname, binding_path, hash_lit)
            }
        }
        other => syn::Error::new_spanned(other, "#[schema] requires a struct or enum")
            .to_compile_error()
            .into(),
    }
}

/// Builds the `BindingArgs` path for a table `Binding` path.
fn binding_args_path(binding_path: &SynPath) -> SynPath {
    let binding_ident = binding_path.segments.last().unwrap().ident.clone();
    let args_ident = format_ident!("{}Args", binding_ident);
    let mut segments = binding_path.segments.clone();
    segments.pop();
    segments.push(syn::PathSegment {
        ident: args_ident,
        arguments: syn::PathArguments::None,
    });
    SynPath {
        leading_colon: binding_path.leading_colon,
        segments,
    }
}

/// Classifies a data-carrying variant field against the supported field shapes.
fn classify_data_field(field: &syn::Field) -> syn::Result<DataFieldKind> {
    if has_schema_skip(field) {
        return Ok(DataFieldKind::Skip);
    }
    let syn::Type::Path(tp) = &field.ty else {
        return Err(syn::Error::new_spanned(
            &field.ty,
            "#[schema] data enums require path-typed or `Option`-wrapped fields",
        ));
    };
    if let Some(inner) = option_inner(tp) {
        if is_string_type(inner) {
            Ok(DataFieldKind::OptionString)
        } else if is_scalar_type(inner) {
            Ok(DataFieldKind::OptionScalar)
        } else {
            Ok(DataFieldKind::Nested { optional: true })
        }
    } else if let Some(inner) = vec_inner(tp) {
        if is_u8_type(inner) {
            Ok(DataFieldKind::Bytes)
        } else if is_string_type(inner) || is_scalar_type(inner) {
            Err(syn::Error::new_spanned(
                &field.ty,
                "#[schema] data enums support `Vec<u8>` and vectors of nested `#[schema]` messages",
            ))
        } else {
            Ok(DataFieldKind::NestedVec)
        }
    } else if is_string_type(&field.ty) {
        Ok(DataFieldKind::String)
    } else if is_scalar_type(&field.ty) {
        Ok(DataFieldKind::Scalar)
    } else {
        Ok(DataFieldKind::Nested { optional: false })
    }
}

fn classify_field(field: &syn::Field, generic_params: &[syn::Ident]) -> FieldKind {
    match &field.ty {
        syn::Type::Path(tp) => {
            if let Some(inner) = option_inner(tp) {
                if type_references_generic(inner, generic_params) {
                    FieldKind::OptionGeneric
                } else if is_scalar_type(inner) {
                    FieldKind::OptionScalar
                } else {
                    FieldKind::Concrete
                }
            } else if type_references_generic(&field.ty, generic_params) {
                FieldKind::Generic
            } else {
                FieldKind::Concrete
            }
        }
        _ => FieldKind::Concrete,
    }
}

fn collect_generic_params(generics: &syn::Generics) -> Vec<syn::Ident> {
    generics
        .params
        .iter()
        .filter_map(|p| {
            if let syn::GenericParam::Type(tp) = p {
                Some(tp.ident.clone())
            } else {
                None
            }
        })
        .collect()
}

/// Returns whether an enum has data-carrying variants (named fields or tuple
/// payloads), distinguishing it from a unit-only enum bound to a FlatBuffers
/// native enum type.
fn data_enum_has_fields(en: &ItemEnum) -> bool {
    en.variants.iter().any(|variant| match &variant.fields {
        syn::Fields::Named(named) => !named.named.is_empty(),
        syn::Fields::Unnamed(_) => true,
        syn::Fields::Unit => false,
    })
}

/// Returns the decode expression for one variant field. Skipped fields decode
/// to their `Default` value; required nested fields decode strictly.
fn decode_data_field(field: &DataField) -> proc_macro2::TokenStream {
    let wire_ident = format_ident!("{}", field.wire_name);
    let wire_name_lit = syn::LitStr::new(&field.wire_name, proc_macro2::Span::call_site());
    let enc = encoding_path();
    match field.kind {
        DataFieldKind::Skip => quote! { ::core::default::Default::default() },
        DataFieldKind::String => quote! { #enc::StringFieldValue::into_owned(view.#wire_ident()) },
        DataFieldKind::Scalar => quote! { view.#wire_ident() },
        DataFieldKind::Bytes => {
            quote! { view.#wire_ident().map(|value| value.bytes().to_vec()).unwrap_or_default() }
        }
        DataFieldKind::OptionString => quote! { view.#wire_ident().map(|value| value.to_string()) },
        DataFieldKind::OptionScalar => {
            quote! { { let value = view.#wire_ident(); (value != 0).then_some(value) } }
        }
        DataFieldKind::Nested { optional: false } => {
            let ty = &field.ty;
            quote! {
                match view.#wire_ident() {
                    Some(value) => #ty::from_flatbuffer(value)?,
                    None => {
                        return ::flatbuffers::InvalidFlatbuffer::new_missing_required(#wire_name_lit);
                    }
                }
            }
        }
        DataFieldKind::Nested { optional: true } => {
            let ty = nested_inner_of_field(field);
            quote! {
                match view.#wire_ident() {
                    Some(value) => Some(#ty::from_flatbuffer(value)?),
                    None => None,
                }
            }
        }
        DataFieldKind::NestedVec => {
            let inner = vec_inner_of_field(field);
            quote! {
                match view.#wire_ident() {
                    Some(value) => value
                        .iter()
                        .map(#inner::from_flatbuffer)
                        .collect::<::std::result::Result<::std::vec::Vec<_>, flatbuffers::InvalidFlatbuffer>>()?,
                    None => ::std::vec::Vec::new(),
                }
            }
        }
    }
}

fn decode_field(field: &syn::Field) -> proc_macro2::TokenStream {
    let id = field.ident.as_ref().unwrap();
    let enc = encoding_path();
    match &field.ty {
        syn::Type::Path(tp) => {
            if let Some(inner) = option_inner(tp) {
                decode_option_field(id, inner)
            } else if let Some(inner) = vec_inner(tp) {
                decode_vec_field(id, inner, true)
            } else {
                let ident = &tp.path.segments.last().unwrap().ident;
                if has_schema_codec(field) {
                    quote! { #id: #enc::FieldDecoder::decode_field(view.#id())? }
                } else if ident == "String" {
                    quote! { #id: #enc::StringFieldValue::into_owned(view.#id()) }
                } else if is_scalar_ident(ident) {
                    quote! { #id: view.#id() }
                } else {
                    // A FlatBuffers native enum field (a unit-only enum bound to
                    // an `enum` type): `view.#id()` already yields the binding
                    // and `from_flatbuffer` is infallible.
                    quote! { #id: #tp::from_flatbuffer(view.#id()) }
                }
            }
        }
        _ => quote! { #id: view.#id() },
    }
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
        return quote! {
            #id: match view.#id() {
                Some(value) => Some(#tp::from_flatbuffer(value)?),
                None => None,
            }
        };
    }

    quote! { #id: view.#id() }
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

    let map_expr = quote! {
        value
            .iter()
            .map(#inner::from_flatbuffer)
            .collect::<::std::result::Result<::std::vec::Vec<_>, flatbuffers::InvalidFlatbuffer>>()?
    };
    if is_required {
        return quote! {
            #id: match view.#id() {
                Some(value) => #map_expr,
                None => ::std::vec::Vec::new(),
            }
        };
    }

    quote! {
        #id: match view.#id() {
            Some(value) => Some(#map_expr),
            None => None,
        }
    }
}

/// Returns the encode statement for one variant field binding, or `None` for a
/// skipped field.
fn encode_data_field(field: &DataField, bind: &syn::Ident) -> Option<proc_macro2::TokenStream> {
    let wire_ident = format_ident!("{}", field.wire_name);
    let statement = match field.kind {
        DataFieldKind::Skip => return None,
        DataFieldKind::String => quote! { args.#wire_ident = Some(builder.create_string(#bind)); },
        DataFieldKind::Scalar => quote! { args.#wire_ident = *#bind; },
        DataFieldKind::Bytes => quote! { args.#wire_ident = Some(builder.create_vector(#bind)); },
        DataFieldKind::OptionString => {
            quote! { args.#wire_ident = #bind.as_ref().map(|value| builder.create_string(value)); }
        }
        DataFieldKind::OptionScalar => {
            quote! { args.#wire_ident = #bind.unwrap_or_default(); }
        }
        DataFieldKind::Nested { optional: false } => {
            quote! { args.#wire_ident = Some(#bind.write_flatbuffer(builder)); }
        }
        DataFieldKind::Nested { optional: true } => {
            quote! { args.#wire_ident = #bind.as_ref().map(|value| value.write_flatbuffer(builder)); }
        }
        DataFieldKind::NestedVec => {
            let offsets_ident = format_ident!("{}_offsets", field.wire_name);
            quote! {
                let #offsets_ident: ::std::vec::Vec<_> = #bind
                    .iter()
                    .map(|item| item.write_flatbuffer(builder))
                    .collect();
                args.#wire_ident = Some(builder.create_vector(&#offsets_ident));
            }
        }
    };
    Some(statement)
}

fn encode_field(field: &syn::Field) -> proc_macro2::TokenStream {
    let id = field.ident.as_ref().unwrap();
    let enc = encoding_path();
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
                        args.#id = #enc::FieldEncoder::encode_field(
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

/// Returns the path prefix for encoding types.
///
/// The types live at the `selium-service` crate root. Inside `selium-service`
/// itself they are referred to as `crate`; every other consumer refers to them
/// through the `selium_service` dependency. We detect this via
/// `CARGO_CRATE_NAME`.
fn encoding_path() -> proc_macro2::TokenStream {
    match std::env::var("CARGO_CRATE_NAME").as_deref() {
        Ok("selium_service") => quote! { crate },
        Ok("selium_wire") => quote! { selium_service },
        Ok("selium_proto_http") => quote! { selium_service },
        Ok("selium_proto_dns") => quote! { selium_service },
        _ => quote! { selium_service },
    }
}

/// Emits the FlatMsg/HasSchema codec for a data-carrying enum bound to a
/// flattened `variant: ubyte` table.
fn expand_data_enum(
    en: ItemEnum,
    fqname: String,
    binding_path: SynPath,
    hash_lit: syn::LitByteStr,
) -> syn::Result<TokenStream> {
    // Cleaned enum: strip schema/tag/field attributes, keep everything else.
    let mut en2 = en.clone();
    en2.attrs = strip_schema_attrs(&en.attrs);
    for variant in &mut en2.variants {
        let attrs = strip_schema_attrs(&variant.attrs);
        variant.attrs = attrs;
        match &mut variant.fields {
            syn::Fields::Named(named) => {
                for field in &mut named.named {
                    let attrs = strip_schema_attrs(&field.attrs);
                    field.attrs = attrs;
                }
            }
            syn::Fields::Unnamed(unnamed) => {
                for field in &mut unnamed.unnamed {
                    let attrs = strip_schema_attrs(&field.attrs);
                    field.attrs = attrs;
                }
            }
            syn::Fields::Unit => {}
        }
    }

    // Assign tags and classify every variant.
    let mut used_tags = std::collections::HashSet::new();
    let mut variants: Vec<DataVariant> = Vec::with_capacity(en.variants.len());
    for (index, variant) in en.variants.iter().enumerate() {
        let tag = match variant_tag(variant)? {
            Some(tag) => tag,
            None => u8::try_from(index).map_err(|_| {
                syn::Error::new_spanned(
                    variant,
                    "too many variants; #[schema] tags must fit within the u8 range",
                )
            })?,
        };
        if !used_tags.insert(tag) {
            return Err(syn::Error::new_spanned(
                variant,
                format!("duplicate #[tag] value {tag}"),
            ));
        }

        let shape = match &variant.fields {
            syn::Fields::Unit => DataVariantShape::Unit,
            syn::Fields::Named(named) => {
                let mut fields = Vec::with_capacity(named.named.len());
                for field in &named.named {
                    let ident = field.ident.clone().expect("named field has an ident");
                    let wire_name = match field_rename(&field.attrs)? {
                        Some(name) => name,
                        None => ident.to_string(),
                    };
                    let kind = classify_data_field(field)?;
                    fields.push(DataField {
                        ident: Some(ident),
                        wire_name,
                        ty: field.ty.clone(),
                        kind,
                    });
                }
                DataVariantShape::Named(fields)
            }
            syn::Fields::Unnamed(unnamed) => {
                let field = unnamed.unnamed.first().ok_or_else(|| {
                    syn::Error::new_spanned(
                        variant,
                        "#[schema] data enum tuple variants require a single field",
                    )
                })?;
                let wire_name = field_rename(&variant.attrs)?.ok_or_else(|| {
                    syn::Error::new_spanned(
                        variant,
                        "single unnamed-field variants require #[field(\"wire_name\")]",
                    )
                })?;
                let kind = classify_data_field(field)?;
                DataVariantShape::Tuple(Box::new(DataField {
                    ident: None,
                    wire_name,
                    ty: field.ty.clone(),
                    kind,
                }))
            }
        };

        variants.push(DataVariant {
            ident: variant.ident.clone(),
            tag,
            shape,
        });
    }

    let enum_ident = &en.ident;
    let schema_ident = format_ident!("{}Schema", enum_ident);
    let fq_lit = fqname.clone();
    let binding_path_ts = quote! { #binding_path };
    let args_path = binding_args_path(&binding_path);
    let enc = encoding_path();
    let unknown_lit = syn::LitStr::new(
        &format!("known {enum_ident} variant"),
        proc_macro2::Span::call_site(),
    );

    // Encode arms.
    let encode_arms = variants.iter().map(|variant| {
        let ident = &variant.ident;
        let tag = variant.tag;
        match &variant.shape {
            DataVariantShape::Unit => quote! {
                Self::#ident => { args.variant = #tag; }
            },
            DataVariantShape::Named(fields) => {
                let pats = fields
                    .iter()
                    .filter(|field| !matches!(field.kind, DataFieldKind::Skip))
                    .map(|field| field.ident.clone().unwrap());
                let stmts = fields
                    .iter()
                    .filter_map(|field| encode_data_field(field, field.ident.as_ref().unwrap()));
                quote! {
                    Self::#ident { #(#pats,)* .. } => {
                        args.variant = #tag;
                        #(#stmts)*
                    }
                }
            }
            DataVariantShape::Tuple(field) => {
                let value = format_ident!("value");
                let stmt = encode_data_field(field, &value).expect("tuple fields are encodable");
                quote! {
                    Self::#ident(value) => {
                        args.variant = #tag;
                        #stmt
                    }
                }
            }
        }
    });

    // Decode arms.
    let decode_arms = variants.iter().map(|variant| {
        let ident = &variant.ident;
        let tag = variant.tag;
        match &variant.shape {
            DataVariantShape::Unit => quote! { #tag => Ok(Self::#ident), },
            DataVariantShape::Named(fields) => {
                let inits = fields.iter().map(|field| {
                    let name = field.ident.as_ref().unwrap();
                    let expr = decode_data_field(field);
                    quote! { #name: #expr }
                });
                quote! { #tag => Ok(Self::#ident { #(#inits),* }), }
            }
            DataVariantShape::Tuple(field) => {
                let expr = decode_data_field(field);
                quote! { #tag => Ok(Self::#ident(#expr)), }
            }
        }
    });

    let expanded = quote! {
        #en2

        #[allow(non_upper_case_globals)]
        pub const #schema_ident: #enc::SchemaDescriptor = #enc::SchemaDescriptor {
            fqname: #fq_lit,
            hash: *#hash_lit,
        };

        impl #enc::HasSchema for #enum_ident {
            const SCHEMA: #enc::SchemaDescriptor = #schema_ident;
        }

        impl #enum_ident {
            pub fn write_flatbuffer<'bldr, A: flatbuffers::Allocator + 'bldr>(
                &self,
                builder: &mut flatbuffers::FlatBufferBuilder<'bldr, A>,
            ) -> flatbuffers::WIPOffset<#binding_path_ts<'bldr>> {
                let mut args = #args_path::default();
                match self {
                    #(#encode_arms)*
                }
                #binding_path_ts::create(builder, &args)
            }

            pub fn from_flatbuffer(
                view: #binding_path_ts<'_>,
            ) -> ::std::result::Result<Self, flatbuffers::InvalidFlatbuffer> {
                match view.variant() {
                    #(#decode_arms)*
                    _ => {
                        return ::flatbuffers::InvalidFlatbuffer::new_missing_required(#unknown_lit);
                    }
                }
            }
        }

        impl #enc::FlatMsg for #enum_ident {
            fn encode(value: &Self) -> Vec<u8> {
                let mut builder = flatbuffers::FlatBufferBuilder::new();
                let root = value.write_flatbuffer(&mut builder);
                builder.finish(root, None);
                builder.finished_data().to_vec()
            }

            fn decode(
                bytes: &[u8],
            ) -> ::std::result::Result<Self, flatbuffers::InvalidFlatbuffer> {
                let view = flatbuffers::root::<#binding_path_ts<'_>>(bytes)?;
                Self::from_flatbuffer(view)
            }
        }
    };

    Ok(expanded.into())
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

    let enc = encoding_path();

    let expanded = quote! {
        #en2

        #[allow(non_upper_case_globals)]
        pub const #schema_ident: #enc::SchemaDescriptor = #enc::SchemaDescriptor {
            fqname: #fq_lit,
            hash: *#hash_lit,
        };

        impl #enc::HasSchema for #enum_ident {
            const SCHEMA: #enc::SchemaDescriptor = #schema_ident;
        }

        impl #enc::FieldEncoder for #enum_ident {
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

fn expand_struct(
    st: ItemStruct,
    fbs_path: String,
    fqname: String,
    binding_path: SynPath,
    hash_lit: syn::LitByteStr,
    wire_ident: Option<syn::Ident>,
) -> TokenStream {
    let generic_params = collect_generic_params(&st.generics);

    // Error: generic struct without wire parameter.
    if !generic_params.is_empty() && wire_ident.is_none() {
        return syn::Error::new_spanned(
            &st.generics,
            "struct has generic type parameters; add wire = WireTypeName to #[schema] to generate a wire type",
        )
        .to_compile_error()
        .into();
    }

    // Wire generation path.
    if let Some(ref wire) = wire_ident {
        return expand_struct_with_wire(
            st,
            fbs_path,
            fqname,
            binding_path,
            hash_lit,
            wire,
            &generic_params,
        );
    }

    // Existing direct-impl path (non-generic structs).
    expand_struct_direct(st, fqname, binding_path, hash_lit)
}

/// Direct-impl path: generates FlatMsg, HasSchema, FieldEncoder, new, write_flatbuffer,
/// from_flatbuffer on the annotated struct itself.
fn expand_struct_direct(
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
    if let syn::Fields::Named(named) = &mut st2.fields {
        for field in &mut named.named {
            let attrs = field
                .attrs
                .iter()
                .filter(|attr| !attr.path().is_ident("schema"))
                .cloned()
                .collect();
            field.attrs = attrs;
        }
    }
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
    let enc = encoding_path();

    let expanded = quote! {
        #st2

        #[allow(non_upper_case_globals)]
        pub const #schema_ident: #enc::SchemaDescriptor = #enc::SchemaDescriptor {
            fqname: #fq_lit,
            hash: *#hash_lit,
        };

        impl #enc::HasSchema for #struct_ident {
            const SCHEMA: #enc::SchemaDescriptor = #schema_ident;
        }

        impl #enc::FieldEncoder for #struct_ident {
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

            pub fn from_flatbuffer(
                view: #binding_path_ts<'_>,
            ) -> ::std::result::Result<Self, flatbuffers::InvalidFlatbuffer> {
                Ok(Self { #( #decode_fields, )* })
            }
        }

        impl #enc::FlatMsg for #struct_ident {
            fn encode(value: &Self) -> Vec<u8> {
                let mut builder = flatbuffers::FlatBufferBuilder::new();
                let root = value.write_flatbuffer(&mut builder);
                builder.finish(root, None);
                builder.finished_data().to_vec()
            }

            fn decode(bytes: &[u8]) -> ::std::result::Result<Self, flatbuffers::InvalidFlatbuffer> {
                let view = flatbuffers::root::<#binding_path_ts<'_>>(bytes)?;
                Self::from_flatbuffer(view)
            }
        }
    };

    expanded.into()
}

/// Wire path: emits the cleaned domain struct, the wire struct (with `#[schema]`),
/// and bridge FlatMsg/HasSchema impls on the domain type.
fn expand_struct_with_wire(
    st: ItemStruct,
    fbs_path: String,
    fqname: String,
    binding_path: SynPath,
    _hash_lit: syn::LitByteStr,
    wire_ident: &syn::Ident,
    generic_params: &[syn::Ident],
) -> TokenStream {
    // Cleaned domain struct (strip #[schema] attribute, keep everything else).
    let mut cleaned = st.clone();
    cleaned.attrs = st
        .attrs
        .iter()
        .filter(|attr| !attr.path().is_ident("schema"))
        .cloned()
        .collect();

    let wire_struct = generate_wire_struct(
        &st,
        wire_ident,
        &fbs_path,
        &fqname,
        &binding_path,
        generic_params,
    );

    let bridge = generate_bridge_impls(&st, wire_ident, generic_params);

    let expanded = quote! {
        #cleaned

        #wire_struct

        #bridge
    };

    expanded.into()
}

/// Reads a `#[field("wire_name")]` rename attribute, if present.
fn field_rename(attrs: &[syn::Attribute]) -> syn::Result<Option<String>> {
    for attr in attrs {
        if attr.path().is_ident("field") {
            let lit: syn::LitStr = attr.parse_args().map_err(|error| {
                syn::Error::new_spanned(attr, format!("#[field] expects a string literal: {error}"))
            })?;
            return Ok(Some(lit.value()));
        }
    }
    Ok(None)
}

fn generate_bridge_impls(
    st: &ItemStruct,
    wire_ident: &syn::Ident,
    generic_params: &[syn::Ident],
) -> proc_macro2::TokenStream {
    let domain_ident = &st.ident;
    let enc = encoding_path();

    let fields = match &st.fields {
        syn::Fields::Named(named) => named.named.iter().collect::<Vec<_>>(),
        _ => panic!("#[schema] wire generation requires named fields"),
    };

    // --- Encode: domain fields → wire constructor args ---
    let encode_exprs: Vec<proc_macro2::TokenStream> = fields
        .iter()
        .map(|f| {
            let field_name = f.ident.as_ref().unwrap();
            let kind = classify_field(f, generic_params);
            match kind {
                FieldKind::Generic => {
                    quote! { #enc::FlatMsg::encode(&value.#field_name) }
                }
                FieldKind::OptionGeneric => {
                    quote! {
                        match &value.#field_name {
                            Some(v) => #enc::FlatMsg::encode(v),
                            None => Vec::new(),
                        }
                    }
                }
                FieldKind::OptionScalar => {
                    quote! { value.#field_name.unwrap_or_default() }
                }
                FieldKind::Concrete => {
                    quote! { value.#field_name.clone() }
                }
            }
        })
        .collect();

    // --- Decode: wire fields → domain field initialisers ---
    let decode_fields: Vec<proc_macro2::TokenStream> = fields
        .iter()
        .map(|f| {
            let field_name = f.ident.as_ref().unwrap();
            let kind = classify_field(f, generic_params);
            match kind {
                FieldKind::Generic => {
                    let wire_field = format_ident!("{}_bytes", field_name);
                    quote! { #field_name: #enc::FlatMsg::decode(&wire.#wire_field)? }
                }
                FieldKind::OptionGeneric => {
                    let wire_field = format_ident!("{}_bytes", field_name);
                    quote! {
                        #field_name: if wire.#wire_field.is_empty() {
                            None
                        } else {
                            Some(#enc::FlatMsg::decode(&wire.#wire_field)?)
                        }
                    }
                }
                FieldKind::OptionScalar => {
                    let ty = &f.ty;
                    // Extract the inner scalar type from Option<T>.
                    let inner_ty = match ty {
                        syn::Type::Path(tp) => option_inner(tp).unwrap(),
                        _ => unreachable!(),
                    };
                    quote! {
                        #field_name: {
                            let v: #inner_ty = wire.#field_name;
                            let zero: #inner_ty = ::std::default::Default::default();
                            if v == zero { None } else { Some(v) }
                        }
                    }
                }
                FieldKind::Concrete => {
                    quote! { #field_name: wire.#field_name }
                }
            }
        })
        .collect();

    // Generic bounds: each type param gets `: FlatMsg`.
    let bounded_params: Vec<proc_macro2::TokenStream> = generic_params
        .iter()
        .map(|gp| quote! { #gp: #enc::FlatMsg })
        .collect();

    let generic_args: Vec<_> = generic_params.iter().collect();

    let wire_schema_ident = format_ident!("{}Schema", wire_ident);

    quote! {
        impl<#(#bounded_params),*> #enc::FlatMsg for #domain_ident<#(#generic_args),*> {
            fn encode(value: &Self) -> Vec<u8> {
                let wire = #wire_ident::new(#(#encode_exprs),*);
                #enc::FlatMsg::encode(&wire)
            }

            fn decode(bytes: &[u8]) -> ::std::result::Result<Self, flatbuffers::InvalidFlatbuffer> {
                let wire: #wire_ident = #enc::FlatMsg::decode(bytes)?;
                Ok(Self {
                    #(#decode_fields),*
                })
            }
        }

        impl<#(#bounded_params),*> #enc::HasSchema for #domain_ident<#(#generic_args),*> {
            const SCHEMA: #enc::SchemaDescriptor = #wire_schema_ident;
        }
    }
}

fn generate_wire_struct(
    st: &ItemStruct,
    wire_ident: &syn::Ident,
    fbs_path: &str,
    fqname: &str,
    binding_path: &SynPath,
    generic_params: &[syn::Ident],
) -> proc_macro2::TokenStream {
    let fields = match &st.fields {
        syn::Fields::Named(named) => named.named.iter().collect::<Vec<_>>(),
        _ => panic!("#[schema] wire generation requires named fields"),
    };

    let wire_fields: Vec<proc_macro2::TokenStream> = fields
        .iter()
        .map(|f| {
            let field_name = f.ident.as_ref().unwrap();
            let kind = classify_field(f, generic_params);
            match kind {
                FieldKind::Generic => {
                    let wire_name = format_ident!("{}_bytes", field_name);
                    quote! { pub #wire_name: Vec<u8> }
                }
                FieldKind::OptionGeneric => {
                    let wire_name = format_ident!("{}_bytes", field_name);
                    quote! { pub #wire_name: Vec<u8> }
                }
                FieldKind::OptionScalar => {
                    // Option<scalar> → scalar (sentinel value)
                    let inner = option_inner(match &f.ty {
                        syn::Type::Path(tp) => tp,
                        _ => unreachable!(),
                    })
                    .unwrap();
                    quote! { pub #field_name: #inner }
                }
                FieldKind::Concrete => {
                    let ty = &f.ty;
                    quote! { pub #field_name: #ty }
                }
            }
        })
        .collect();

    // Copy derive attributes from the domain struct.
    let derive_attrs: Vec<_> = st
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("derive"))
        .collect();

    let fbs_path_lit = fbs_path;
    let fqname_lit = fqname;

    quote! {
        #(#derive_attrs)*
        #[schema(
            path = #fbs_path_lit,
            ty = #fqname_lit,
            binding = #binding_path
        )]
        pub struct #wire_ident {
            #(#wire_fields),*
        }
    }
}

/// Returns whether a field carries `#[schema(codec)]`, marking a foreign
/// (non-schema) field type whose decode is strict and fallible via
/// `FieldDecoder`.
fn has_schema_codec(field: &syn::Field) -> bool {
    field.attrs.iter().any(|attr| {
        attr.path().is_ident("schema")
            && matches!(
                attr.meta,
                syn::Meta::List(ref list) if list.tokens.to_string().trim() == "codec"
            )
    })
}

/// Returns whether a field carries `#[schema(skip)]`.
fn has_schema_skip(field: &syn::Field) -> bool {
    field.attrs.iter().any(|attr| {
        attr.path().is_ident("schema")
            && matches!(
                attr.meta,
                syn::Meta::List(ref list) if list.tokens.to_string().trim() == "skip"
            )
    })
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

fn is_scalar_type(ty: &syn::Type) -> bool {
    if let syn::Type::Path(tp) = ty {
        if let Some(ident) = tp.path.get_ident() {
            return is_scalar_ident(ident);
        }
    }
    false
}

fn is_string_type(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Path(tp) if tp.path.get_ident().is_some_and(|id| id == "String"))
}

fn is_u8_type(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Path(tp) if tp.path.get_ident().is_some_and(|id| id == "u8"))
}

/// Returns the inner type of an `Option<T>` data-enum field.
fn nested_inner_of_field(field: &DataField) -> &syn::Type {
    let syn::Type::Path(tp) = &field.ty else {
        unreachable!("optional nested field is a path type");
    };
    option_inner(tp).expect("optional nested field is an Option")
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

/// Strips the schema-related attributes (`schema`, `tag`, `field`) from a list,
/// keeping doc comments and any user attributes.
fn strip_schema_attrs(attrs: &[syn::Attribute]) -> Vec<syn::Attribute> {
    attrs
        .iter()
        .filter(|attr| {
            !(attr.path().is_ident("schema")
                || attr.path().is_ident("tag")
                || attr.path().is_ident("field"))
        })
        .cloned()
        .collect()
}

fn type_references_generic(ty: &syn::Type, generic_params: &[syn::Ident]) -> bool {
    if let syn::Type::Path(tp) = ty {
        for seg in &tp.path.segments {
            if generic_params.iter().any(|gp| gp == &seg.ident) {
                return true;
            }
        }
    }
    false
}

/// Reads the `#[tag(n)]` override on a variant, if present.
fn variant_tag(variant: &syn::Variant) -> syn::Result<Option<u8>> {
    for attr in &variant.attrs {
        if attr.path().is_ident("tag") {
            let lit: syn::LitInt = attr.parse_args().map_err(|error| {
                syn::Error::new_spanned(attr, format!("#[tag] expects an integer literal: {error}"))
            })?;
            let tag = lit.base10_parse::<u8>().map_err(|_| {
                syn::Error::new_spanned(attr, "tag must be within the u8 range (0..=255)")
            })?;
            return Ok(Some(tag));
        }
    }
    Ok(None)
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

/// Returns the inner element type of a `Vec<T>` data-enum field.
fn vec_inner_of_field(field: &DataField) -> &syn::Type {
    let syn::Type::Path(tp) = &field.ty else {
        unreachable!("NestedVec field is a path type");
    };
    vec_inner(tp).expect("NestedVec field is a Vec")
}
