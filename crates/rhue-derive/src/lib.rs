//! `#[derive(UserData)]` for Rhue. Use it through the `rhue` crate, which
//! re-exports it; see `rhue::UserData` for what it generates.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as Tokens;
use quote::quote;
use syn::{Data, DeriveInput, Fields, LitStr, parse_macro_input};

#[proc_macro_derive(UserData, attributes(rhue))]
pub fn derive_user_data(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// What `#[rhue(...)]` says about a field or the type.
#[derive(Default)]
struct Options {
    name: Option<String>,
    skip: bool,
    readonly: bool,
}

fn options(attrs: &[syn::Attribute], on_field: bool) -> syn::Result<Options> {
    let mut o = Options::default();
    for attr in attrs.iter().filter(|a| a.path().is_ident("rhue")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                o.name = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if on_field && meta.path.is_ident("skip") {
                o.skip = true;
            } else if on_field && meta.path.is_ident("readonly") {
                o.readonly = true;
            } else {
                let allowed = if on_field {
                    "`name = \"...\"`, `skip` or `readonly`"
                } else {
                    "`name = \"...\"`"
                };
                return Err(meta.error(format!("unknown rhue option; expected {allowed}")));
            }
            Ok(())
        })?;
    }
    Ok(o)
}

fn expand(input: DeriveInput) -> syn::Result<Tokens> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "UserData can't be derived for generic types; implement rhue::UserData by hand",
        ));
    }
    let ident = &input.ident;
    let type_name = options(&input.attrs, false)?
        .name
        .unwrap_or_else(|| ident.to_string());

    let fields = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => named.named.iter().collect(),
            // Tuple and unit structs have no named fields to expose; their
            // methods are added with `register_type`.
            Fields::Unnamed(_) | Fields::Unit => Vec::new(),
        },
        Data::Enum(_) | Data::Union(_) => {
            return Err(syn::Error::new_spanned(
                ident,
                "UserData can only be derived for structs",
            ));
        }
    };

    let mut registrations = Vec::new();
    for field in fields {
        let o = options(&field.attrs, true)?;
        if o.skip {
            continue;
        }
        let member = field.ident.as_ref().expect("named field");
        let ty = &field.ty;
        let name = o.name.unwrap_or_else(|| member.to_string());
        registrations.push(quote! {
            t.getter(#name, |s: &Self| -> #ty { ::core::clone::Clone::clone(&s.#member) });
        });
        if !o.readonly {
            registrations.push(quote! {
                t.setter(#name, |s: &mut Self, v: #ty| s.#member = v);
            });
        }
    }

    Ok(quote! {
        impl ::rhue::UserData for #ident {
            const NAME: &'static str = #type_name;

            #[allow(unused_variables)]
            fn register(t: &mut ::rhue::TypeBuilder<'_, Self>) {
                #(#registrations)*
            }
        }
    })
}
