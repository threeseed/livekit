//! `#[derive(ConfigSchema)]`: the reflection the Go config layer gets from
//! `reflect` and yaml struct tags.
//!
//! The Go server walks its config struct with `reflect` twice: once to
//! generate one CLI flag per YAML field (`config.GenerateCLIFlags`), and once
//! inside `yaml.Decoder.KnownFields` to reject unknown keys. Rust has no
//! runtime reflection, so the same walk is produced at compile time here: the
//! derive emits a `&'static StructSchema` describing every field's YAML key,
//! its scalar kind, and whether it is flattened (Go's `yaml:",inline"`).
//!
//! Attributes read from the field:
//!
//! | Attribute | Effect |
//! |---|---|
//! | `#[serde(rename = "x")]` | YAML key is `x` rather than the field name |
//! | `#[serde(flatten)]` | Go's `yaml:",inline"`: the nested keys live in the parent |
//! | `#[serde(skip)]` | field is absent from YAML entirely (Go's `yaml:"-"`) |
//! | `#[config(opaque)]` | a leaf whose contents are not walked (proto messages) |

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Data, DeriveInput, Fields, GenericArgument, PathArguments, Type, parse_macro_input};

/// Derives [`HasSchema`](../lk_config/schema/trait.HasSchema.html) for a config
/// struct: a compile-time description of its YAML surface.
#[proc_macro_derive(ConfigSchema, attributes(config))]
pub fn derive_config_schema(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(&input) {
        Ok(ts) => ts.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn expand(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let ident = &input.ident;
    let name = ident.to_string();

    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            ident,
            "ConfigSchema is only supported on structs",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            ident,
            "ConfigSchema requires named fields",
        ));
    };

    let mut entries = Vec::new();
    for field in &fields.named {
        let Some(field_ident) = field.ident.as_ref() else {
            continue;
        };
        let attrs = FieldAttrs::parse(&field.attrs)?;
        if attrs.skip {
            continue;
        }
        let key = attrs.rename.unwrap_or_else(|| field_ident.to_string());
        let inline = attrs.flatten;
        let kind = if attrs.opaque {
            quote!(::lk_config::schema::Kind::Opaque)
        } else {
            kind_of(&field.ty)
        };
        entries.push(quote! {
            ::lk_config::schema::FieldSchema {
                name: #key,
                inline: #inline,
                kind: #kind,
            }
        });
    }

    Ok(quote! {
        impl ::lk_config::schema::HasSchema for #ident {
            fn schema() -> &'static ::lk_config::schema::StructSchema {
                static SCHEMA: ::lk_config::schema::StructSchema =
                    ::lk_config::schema::StructSchema {
                        name: #name,
                        fields: &[#(#entries),*],
                    };
                &SCHEMA
            }
        }
    })
}

#[derive(Default)]
struct FieldAttrs {
    rename: Option<String>,
    flatten: bool,
    skip: bool,
    opaque: bool,
}

impl FieldAttrs {
    fn parse(attrs: &[syn::Attribute]) -> syn::Result<Self> {
        let mut out = Self::default();
        for attr in attrs {
            if attr.path().is_ident("serde") {
                // `parse_nested_meta` on serde attributes must not fail the build on
                // the shapes this macro does not care about (`default = "..."`,
                // `with = "..."`), so unknown keys are consumed and ignored.
                let _ = attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("rename") {
                        let value: syn::LitStr = meta.value()?.parse()?;
                        out.rename = Some(value.value());
                    } else if meta.path.is_ident("flatten") {
                        out.flatten = true;
                    } else if meta.path.is_ident("skip") {
                        out.skip = true;
                    } else if meta.input.peek(syn::Token![=]) {
                        let _: syn::Expr = meta.value()?.parse()?;
                    }
                    Ok(())
                });
            } else if attr.path().is_ident("config") {
                attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("opaque") {
                        out.opaque = true;
                        Ok(())
                    } else {
                        Err(meta.error("unknown lk-config attribute"))
                    }
                })?;
            }
        }
        Ok(out)
    }
}

/// Maps a field type to the schema kind the CLI generator and the strict-mode
/// walker switch on. Mirrors the `reflect.Kind` switch in
/// `config.GenerateCLIFlags`, including its rule that sequences, maps and
/// structs produce no flag of their own.
fn kind_of(ty: &Type) -> TokenStream2 {
    let Some(segment) = last_segment(ty) else {
        return quote!(::lk_config::schema::Kind::Opaque);
    };
    let ident = segment.ident.to_string();
    match ident.as_str() {
        "bool" => quote!(::lk_config::schema::Kind::Bool),
        "String" => quote!(::lk_config::schema::Kind::Str),
        "u8" | "u16" | "u32" | "u64" | "usize" => quote!(::lk_config::schema::Kind::Uint),
        "i8" | "i16" | "i32" | "i64" | "isize" => quote!(::lk_config::schema::Kind::Int),
        "f32" | "f64" => quote!(::lk_config::schema::Kind::Float),
        "GoDuration" => quote!(::lk_config::schema::Kind::Duration),
        "Option" => match generic_arg(segment) {
            Some(inner) => kind_of(inner),
            None => quote!(::lk_config::schema::Kind::Opaque),
        },
        "Vec" => {
            let elem = generic_arg(segment).map(nested_schema_fn);
            match elem {
                Some(Some(path)) => quote!(::lk_config::schema::Kind::Seq(Some(#path))),
                _ => quote!(::lk_config::schema::Kind::Seq(None)),
            }
        }
        "BTreeMap" | "HashMap" => {
            let value = generic_args(segment).nth(1).map(nested_schema_fn);
            match value {
                Some(Some(path)) => quote!(::lk_config::schema::Kind::Map(Some(#path))),
                _ => quote!(::lk_config::schema::Kind::Map(None)),
            }
        }
        _ => {
            let path = &segment.ident;
            quote!(::lk_config::schema::Kind::Nested(
                <#path as ::lk_config::schema::HasSchema>::schema
            ))
        }
    }
}

/// The `fn() -> &'static StructSchema` for a sequence element or map value, or
/// `None` when the element is a scalar and has no schema of its own.
fn nested_schema_fn(ty: &Type) -> Option<TokenStream2> {
    let segment = last_segment(ty)?;
    let ident = segment.ident.to_string();
    match ident.as_str() {
        "bool" | "String" | "u8" | "u16" | "u32" | "u64" | "usize" | "i8" | "i16" | "i32"
        | "i64" | "isize" | "f32" | "f64" | "GoDuration" | "Value" => None,
        _ => {
            let path = &segment.ident;
            Some(quote!(<#path as ::lk_config::schema::HasSchema>::schema))
        }
    }
}

fn last_segment(ty: &Type) -> Option<&syn::PathSegment> {
    match ty {
        Type::Path(path) => path.path.segments.last(),
        _ => None,
    }
}

fn generic_arg(segment: &syn::PathSegment) -> Option<&Type> {
    generic_args(segment).next()
}

fn generic_args(segment: &syn::PathSegment) -> impl Iterator<Item = &Type> {
    let args = match &segment.arguments {
        PathArguments::AngleBracketed(args) => Some(&args.args),
        _ => None,
    };
    args.into_iter().flatten().filter_map(|arg| match arg {
        GenericArgument::Type(ty) => Some(ty),
        _ => None,
    })
}
