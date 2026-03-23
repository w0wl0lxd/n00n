//! Derives `schema()` (JSON Schema for the API) and `parse_input()` (deserialize from API JSON)
//! from struct fields. `#[param(description = "...")]` maps to JSON Schema description.
//! Optional fields (`Option<T>`) are excluded from `required`. `Vec` fields emit `"type": "array"`.

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

    for field in &fields.named {
        let field_name = field.ident.as_ref().unwrap();
        let field_ty = &field.ty;
        let field_str = field_name.to_string();
        let desc = param_description(&field.attrs).unwrap_or_default();
        let json_type = json_type_str(field_ty);
        let optional = is_option(field_ty);

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

        if optional {
            field_extractions.push(quote! {
                let #field_name: #field_ty = {
                    let raw = input.get(#field_str).cloned().unwrap_or(serde_json::Value::Null);
                    if raw.is_null() {
                        crate::tools::rescue_field(input, #field_str)
                            .map(|v| crate::tools::deserialize_with_coercion(&v, #field_str, #json_type))
                            .transpose()?
                    } else {
                        Some(crate::tools::deserialize_with_coercion(&raw, #field_str, #json_type)?)
                    }
                };
            });
        } else {
            field_extractions.push(quote! {
                let #field_name: #field_ty = {
                    let raw = input.get(#field_str).cloned().or_else(|| {
                        crate::tools::rescue_field(input, #field_str)
                    }).ok_or_else(|| format!("missing required field '{}'", #field_str))?;
                    crate::tools::deserialize_with_coercion(&raw, #field_str, #json_type)?
                };
            });
        }
    }

    let field_names: Vec<&Ident> = fields
        .named
        .iter()
        .map(|f| f.ident.as_ref().unwrap())
        .collect();

    let expanded = quote! {
        impl #name {
            pub(crate) fn schema() -> serde_json::Value {
                let mut props = serde_json::Map::new();
                #(#prop_entries)*
                let mut required = Vec::<String>::new();
                #(#required_entries)*
                serde_json::json!({
                    "type": "object",
                    "properties": serde_json::Value::Object(props),
                    "required": required,
                    "additionalProperties": false
                })
            }

            pub(crate) fn parse_input(input: &serde_json::Value) -> Result<Self, String> {
                #(#field_extractions)*
                Ok(Self { #(#field_names),* })
            }
        }
    };

    expanded.into()
}
