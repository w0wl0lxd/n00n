//! Derive macros for tool schemas.
//!
//! - `Tool`: generates `schema()`, `parse_input()`, `schema_hint()` for top-level tool structs.
//! - `Args`: generates `item_schema()` for structs used as `Vec<T>` items in tool schemas.
//! - `ArgEnum`: generates `item_schema()` for `#[serde(rename_all = "snake_case")]` enums.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    Attribute, Data, DeriveInput, Expr, Fields, GenericArgument, Ident, Lit, Meta, PathArguments,
    Type, parse_macro_input,
};

fn param_description(attrs: &[Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        if !attr.path().is_ident("param") {
            return None;
        }
        let nested: Meta = attr.parse_args().ok()?;
        if let Meta::NameValue(nv) = nested
            && nv.path.is_ident("description")
            && let Expr::Lit(expr_lit) = &nv.value
            && let Lit::Str(lit) = &expr_lit.lit
        {
            return Some(lit.value());
        }
        None
    })
}

fn inner_type<'a>(ty: &'a Type, wrapper: &str) -> Option<&'a Type> {
    if let Type::Path(tp) = ty
        && let Some(seg) = tp.path.segments.last()
        && seg.ident == wrapper
        && let PathArguments::AngleBracketed(args) = &seg.arguments
        && let Some(GenericArgument::Type(inner)) = args.args.first()
    {
        return Some(inner);
    }
    None
}

fn is_option(ty: &Type) -> bool {
    inner_type(ty, "Option").is_some()
}

fn unwrapped_type(ty: &Type) -> &Type {
    inner_type(ty, "Option").unwrap_or(ty)
}

fn json_type_str(ty: &Type) -> &'static str {
    let ty = unwrapped_type(ty);
    if let Type::Path(tp) = ty
        && let Some(seg) = tp.path.segments.last()
    {
        return match seg.ident.to_string().as_str() {
            "String" | "str" => "string",
            "bool" => "boolean",
            "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "i8" | "i16" | "i32" | "i64"
            | "i128" | "isize" => "integer",
            "f32" | "f64" => "number",
            "Vec" => "array",
            _ => "object",
        };
    }
    "object"
}

fn vec_item_schema(ty: &Type) -> TokenStream2 {
    let inner = unwrapped_type(ty);
    if let Some(item_ty) = inner_type(inner, "Vec") {
        let item_json_type = json_type_str(item_ty);
        if item_json_type == "object" {
            return quote! { #item_ty::item_schema() };
        }
        return quote! { serde_json::json!({ "type": #item_json_type }) };
    }
    quote! { serde_json::json!({}) }
}

fn has_serde_default(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("serde") {
            return false;
        }
        let Ok(nested) = attr.parse_args::<Meta>() else {
            return false;
        };
        matches!(nested, Meta::Path(p) if p.is_ident("default"))
    })
}

fn to_snake_case(s: &str) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn is_plain_object(ty: &Type) -> bool {
    if let Type::Path(tp) = ty
        && let Some(seg) = tp.path.segments.last()
    {
        return seg.ident == "Value";
    }
    false
}

fn field_schema_token(field_ty: &Type) -> TokenStream2 {
    let json_type = json_type_str(field_ty);
    let unwrapped = unwrapped_type(field_ty);
    if json_type == "array" {
        let item_schema = vec_item_schema(unwrapped);
        quote! {
            serde_json::json!({
                "type": "array",
                "items": #item_schema
            })
        }
    } else if json_type == "object" && !is_plain_object(unwrapped) {
        quote! { #unwrapped::item_schema() }
    } else {
        quote! { serde_json::json!({ "type": #json_type }) }
    }
}

#[proc_macro_derive(ArgEnum)]
pub fn derive_arg_enum(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    let Data::Enum(data) = &input.data else {
        return syn::Error::new_spanned(name, "ArgEnum can only be derived on enums")
            .to_compile_error()
            .into();
    };

    let variants: Vec<String> = data
        .variants
        .iter()
        .map(|v| to_snake_case(&v.ident.to_string()))
        .collect();

    let expanded = quote! {
        impl #name {
            pub fn item_schema() -> serde_json::Value {
                serde_json::json!({
                    "type": "string",
                    "enum": [#(#variants),*]
                })
            }
        }
    };
    expanded.into()
}

#[proc_macro_derive(Args, attributes(param))]
pub fn derive_args(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    let Data::Struct(data) = &input.data else {
        return syn::Error::new_spanned(name, "Args can only be derived on structs")
            .to_compile_error()
            .into();
    };
    let Fields::Named(fields) = &data.fields else {
        return syn::Error::new_spanned(name, "Args requires named fields")
            .to_compile_error()
            .into();
    };

    let mut prop_entries = Vec::new();
    let mut required_entries = Vec::new();

    for field in &fields.named {
        let field_name = field.ident.as_ref().unwrap();
        let field_ty = &field.ty;
        let field_str = field_name.to_string();
        let desc = param_description(&field.attrs).unwrap_or_default();
        let optional = is_option(field_ty) || has_serde_default(&field.attrs);
        let base = field_schema_token(field_ty);

        prop_entries.push(quote! {
            let mut entry = #base;
            if let serde_json::Value::Object(ref mut m) = entry {
                if !#desc.is_empty() {
                    m.insert("description".to_string(), serde_json::Value::String(#desc.to_string()));
                }
            }
            props.insert(#field_str.to_string(), entry);
        });

        if !optional {
            required_entries.push(quote! { required.push(#field_str.to_string()); });
        }
    }

    let expanded = quote! {
        impl #name {
            pub fn item_schema() -> serde_json::Value {
                let mut props = serde_json::Map::new();
                #(#prop_entries)*
                let mut required = Vec::<String>::new();
                #(#required_entries)*
                serde_json::json!({
                    "type": "object",
                    "required": required,
                    "properties": serde_json::Value::Object(props),
                    "additionalProperties": false
                })
            }
        }
    };
    expanded.into()
}

#[proc_macro_derive(Tool, attributes(param))]
pub fn derive_tool(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    let Data::Struct(data) = &input.data else {
        return syn::Error::new_spanned(name, "Tool can only be derived on structs")
            .to_compile_error()
            .into();
    };
    let Fields::Named(fields) = &data.fields else {
        return syn::Error::new_spanned(name, "Tool requires named fields")
            .to_compile_error()
            .into();
    };

    let mut prop_entries = Vec::new();
    let mut required_entries = Vec::new();
    let mut field_extractions = Vec::new();
    let mut schema_hint_parts = Vec::new();

    for field in &fields.named {
        let field_name = field.ident.as_ref().unwrap();
        let field_ty = &field.ty;
        let field_str = field_name.to_string();
        let desc = param_description(&field.attrs).unwrap_or_default();
        let json_type = json_type_str(field_ty);
        let optional = is_option(field_ty) || has_serde_default(&field.attrs);

        if json_type == "array" {
            let item_schema = vec_item_schema(field_ty);
            prop_entries.push(quote! {
                props.insert(#field_str.to_string(), serde_json::json!({
                    "type": "array",
                    "description": #desc,
                    "items": #item_schema
                }));
            });
        } else {
            prop_entries.push(quote! {
                props.insert(#field_str.to_string(), serde_json::json!({
                    "type": #json_type,
                    "description": #desc
                }));
            });
        }

        if !optional {
            required_entries.push(quote! { required.push(#field_str.to_string()); });
        }

        let hint_suffix = if optional { "?" } else { "" };
        let type_hint = match json_type {
            "string" => "str",
            "integer" => "int",
            "boolean" => "bool",
            "number" => "num",
            "array" => "[...]",
            _ => "obj",
        };
        schema_hint_parts.push(format!("{}{}: {}", field_str, hint_suffix, type_hint));

        if optional {
            field_extractions.push(quote! {
                let #field_name: #field_ty = input
                    .get(#field_str)
                    .filter(|v| !v.is_null())
                    .map(|v| crate::tools::deserialize_with_coercion(v, #field_str, #json_type))
                    .transpose()?;
            });
        } else {
            field_extractions.push(quote! {
                let #field_name: #field_ty = {
                    let raw = input.get(#field_str).filter(|v| !v.is_null()).ok_or_else(|| format!("The required parameter '{}' is missing. Expected: {}", #field_str, Self::schema_hint()))?;
                    crate::tools::deserialize_with_coercion(raw, #field_str, #json_type)?
                };
            });
        }
    }

    let field_names: Vec<&Ident> = fields
        .named
        .iter()
        .map(|f| f.ident.as_ref().unwrap())
        .collect();

    let schema_hint_str = format!("{{ {} }}", schema_hint_parts.join(", "));

    let expanded = quote! {
        impl #name {
            pub(crate) fn schema() -> serde_json::Value {
                let mut props = serde_json::Map::new();
                #(#prop_entries)*
                let mut required = Vec::<String>::new();
                #(#required_entries)*
                serde_json::json!({
                    "type": "object",
                    "required": required,
                    "properties": serde_json::Value::Object(props),
                    "additionalProperties": false
                })
            }

            pub(crate) fn schema_hint() -> &'static str {
                #schema_hint_str
            }

            pub(crate) fn parse_input(input: &serde_json::Value) -> Result<Self, String> {
                let input = &crate::tools::sanitize_tool_input(input);
                #(#field_extractions)*
                Ok(Self { #(#field_names),* })
            }
        }
    };

    expanded.into()
}
