use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{DeriveInput, Expr, Fields, Ident, LitStr, Token, Type, parse_macro_input, parse_str};

struct ConfigAttr {
    key: Ident,
    value: ConfigAttrValue,
}

enum ConfigAttrValue {
    Flag,
    Str(LitStr),
    Expr(Box<Expr>),
}

impl Parse for ConfigAttr {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let key: Ident = input.parse()?;
        if !input.peek(Token![=]) {
            return Ok(Self {
                key,
                value: ConfigAttrValue::Flag,
            });
        }
        input.parse::<Token![=]>()?;
        if input.peek(LitStr) {
            let lit: LitStr = input.parse()?;
            Ok(Self {
                key,
                value: ConfigAttrValue::Str(lit),
            })
        } else {
            let expr: Expr = input.parse()?;
            Ok(Self {
                key,
                value: ConfigAttrValue::Expr(Box::new(expr)),
            })
        }
    }
}

struct StructAttrs {
    section: String,
    fields_only: bool,
}

struct FieldAttrs {
    skip: bool,
    default: Option<Expr>,
    default_doc: Option<LitStr>,
    min: Option<Expr>,
    desc: Option<String>,
    key: Option<String>,
    ty_override: Option<String>,
    val: Option<String>,
}

fn parse_struct_attrs(input: &DeriveInput) -> syn::Result<StructAttrs> {
    let mut section = None;
    let mut fields_only = false;

    for attr in &input.attrs {
        if !attr.path().is_ident("config") {
            continue;
        }
        let nested = attr.parse_args_with(Punctuated::<ConfigAttr, Token![,]>::parse_terminated)?;
        for item in nested {
            match item.key.to_string().as_str() {
                "section" => {
                    if let ConfigAttrValue::Str(lit) = item.value {
                        section = Some(lit.value());
                    }
                }
                "fields_only" => fields_only = true,
                other => {
                    return Err(syn::Error::new(
                        item.key.span(),
                        format!("unknown struct-level config attribute: {other}"),
                    ));
                }
            }
        }
    }

    let section = section.ok_or_else(|| {
        syn::Error::new(input.ident.span(), "missing #[config(section = \"...\")]")
    })?;

    Ok(StructAttrs {
        section,
        fields_only,
    })
}

fn parse_field_attrs(field: &syn::Field) -> syn::Result<FieldAttrs> {
    let mut attrs = FieldAttrs {
        skip: false,
        default: None,
        default_doc: None,
        min: None,
        desc: None,
        key: None,
        ty_override: None,
        val: None,
    };

    for attr in &field.attrs {
        if !attr.path().is_ident("config") {
            continue;
        }
        let nested = attr.parse_args_with(Punctuated::<ConfigAttr, Token![,]>::parse_terminated)?;
        for item in nested {
            match item.key.to_string().as_str() {
                "skip" => attrs.skip = true,
                "default" => match item.value {
                    ConfigAttrValue::Str(lit) => {
                        attrs.default = Some(parse_str(&lit.value())?);
                    }
                    ConfigAttrValue::Expr(expr) => attrs.default = Some(*expr),
                    ConfigAttrValue::Flag => {
                        return Err(syn::Error::new(item.key.span(), "default requires a value"));
                    }
                },
                "default_doc" => {
                    if let ConfigAttrValue::Str(lit) = item.value {
                        attrs.default_doc = Some(lit);
                    }
                }
                "min" => {
                    if let ConfigAttrValue::Expr(expr) = item.value {
                        attrs.min = Some(*expr);
                    }
                }
                "desc" => {
                    if let ConfigAttrValue::Str(lit) = item.value {
                        attrs.desc = Some(lit.value());
                    }
                }
                "key" => {
                    if let ConfigAttrValue::Str(lit) = item.value {
                        attrs.key = Some(lit.value());
                    }
                }
                "ty" => {
                    if let ConfigAttrValue::Str(lit) = item.value {
                        attrs.ty_override = Some(lit.value());
                    }
                }
                "val" => {
                    if let ConfigAttrValue::Str(lit) = item.value {
                        attrs.val = Some(lit.value());
                    }
                }
                other => {
                    return Err(syn::Error::new(
                        item.key.span(),
                        format!("unknown field-level config attribute: {other}"),
                    ));
                }
            }
        }
    }

    Ok(attrs)
}

fn config_value_expr(ty_name: &str, default: Option<&Expr>) -> syn::Result<TokenStream2> {
    match ty_name {
        "bool" => {
            let val = default.ok_or_else(|| {
                syn::Error::new(
                    proc_macro2::Span::call_site(),
                    "bool field requires default",
                )
            })?;
            Ok(quote! { ConfigValue::Bool(#val) })
        }
        "u32" | "u64" | "usize" => {
            let val = default.ok_or_else(|| {
                syn::Error::new(
                    proc_macro2::Span::call_site(),
                    "numeric field requires default",
                )
            })?;
            Ok(quote! { ConfigValue::U64(#val as u64) })
        }
        "String" => Ok(quote! { ConfigValue::Str("none") }),
        other => Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            format!("unsupported config type: {other}"),
        )),
    }
}

fn type_to_name(ty: &Type) -> String {
    quote!(#ty).to_string().replace(' ', "")
}

#[proc_macro_derive(ConfigSection, attributes(config))]
pub fn derive_config_section(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match derive_impl(&input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn derive_impl(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let struct_attrs = parse_struct_attrs(input)?;
    let name = &input.ident;

    let fields = match &input.data {
        syn::Data::Struct(data) => match &data.fields {
            Fields::Named(named) => &named.named,
            _ => return Err(syn::Error::new(name.span(), "only named fields supported")),
        },
        _ => return Err(syn::Error::new(name.span(), "only structs supported")),
    };

    let field_data = collect_field_data(fields)?;
    let section = &struct_attrs.section;

    let (fields_entries, validate_checks) = generate_field_entries(&field_data, section)?;

    let fields_const = quote! {
        pub const FIELDS: &[ConfigField] = &[
            #(#fields_entries),*
        ];
    };

    let validate_fn = quote! {
        pub fn validate(&self) -> Result<(), ConfigError> {
            #(#validate_checks)*
            Ok(())
        }
    };

    if struct_attrs.fields_only {
        Ok(quote! {
            impl #name {
                #fields_const
                #validate_fn
            }
        })
    } else {
        let default_fields = generate_default_fields(&field_data)?;

        Ok(quote! {
            impl Default for #name {
                fn default() -> Self {
                    Self {
                        #(#default_fields),*
                    }
                }
            }

            impl #name {
                #fields_const
                #validate_fn
            }
        })
    }
}

fn collect_field_data(
    fields: &syn::punctuated::Punctuated<syn::Field, syn::token::Comma>,
) -> syn::Result<Vec<(Ident, FieldAttrs, Type)>> {
    let mut field_data = Vec::new();
    for field in fields {
        let ident = field
            .ident
            .as_ref()
            .ok_or_else(|| syn::Error::new(field.span(), "field must have an identifier"))?;
        let attrs = parse_field_attrs(field)?;
        let ty = &field.ty;
        field_data.push((ident.clone(), attrs, ty.clone()));
    }
    Ok(field_data)
}

fn generate_field_entries(
    field_data: &[(Ident, FieldAttrs, Type)],
    section: &str,
) -> syn::Result<(Vec<TokenStream2>, Vec<TokenStream2>)> {
    let mut fields_entries = Vec::new();
    let mut validate_checks = Vec::new();

    for (ident, attrs, ty) in field_data {
        if attrs.skip {
            continue;
        }

        let ident_string = ident.to_string();
        let field_name = attrs
            .key
            .as_deref()
            .unwrap_or_else(|| ident_string.as_str());
        let ty_string = type_to_name(ty);
        let ty_name = attrs
            .ty_override
            .as_deref()
            .unwrap_or_else(|| ty_string.as_str());
        let desc = attrs.desc.as_deref().unwrap_or_else(|| "");
        let default_expr = match &attrs.default_doc {
            Some(doc) => quote! { ConfigValue::Str(#doc) },
            None => config_value_expr(ty_name, attrs.default.as_ref())?,
        };
        let min_expr = if let Some(m) = &attrs.min {
            quote! { Some(#m as u64) }
        } else {
            quote! { None }
        };

        fields_entries.push(quote! {
            ConfigField {
                name: #field_name,
                ty: #ty_name,
                default: #default_expr,
                min: #min_expr,
                description: #desc,
            }
        });

        if let Some(min) = &attrs.min {
            let val_expr: TokenStream2 = if let Some(val_str) = &attrs.val {
                val_str.parse().map_err(|e| {
                    syn::Error::new(ident.span(), format!("invalid val expression: {e}"))
                })?
            } else {
                quote! { self.#ident }
            };
            let ident_str = ident.to_string();
            let field_name_for_check = attrs.key.as_deref().unwrap_or_else(|| ident_str.as_str());
            validate_checks.push(quote! {
                check(#section, #field_name_for_check, #val_expr as u64, #min as u64)?;
            });
        }
    }

    Ok((fields_entries, validate_checks))
}

fn generate_default_fields(
    field_data: &[(Ident, FieldAttrs, Type)],
) -> syn::Result<Vec<TokenStream2>> {
    let mut default_fields = Vec::new();
    for (ident, attrs, _ty) in field_data {
        let default = attrs.default.as_ref().ok_or_else(|| {
            syn::Error::new(
                ident.span(),
                format!("field `{ident}` requires a default value in full mode"),
            )
        })?;
        default_fields.push(quote! { #ident: #default });
    }
    Ok(default_fields)
}
