//! `#[derive(velt_native::Value)]`: a Rust struct with the layout of a Velt `struct`, so it can be
//! a parameter (`&S`), a result (`S`, `Result<S, Error>`), an array element or another struct's
//! field. Velt lays a struct's fields out in declaration order at natural alignment, as
//! `#[repr(C)]` does; the signature names every field, so the compiler refuses a Velt struct whose
//! fields differ in name, order or type.

use proc_macro2::TokenStream as Tokens;
use quote::quote;
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Fields, LitStr};

/// `snake_case` → `camelCase` (Velt's field names).
fn camel(name: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for c in name.trim_start_matches("r#").chars() {
        if c == '_' && !out.is_empty() {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn is_repr_c(input: &DeriveInput) -> bool {
    input.attrs.iter().any(|a| {
        if !a.path().is_ident("repr") {
            return false;
        }
        let mut c = false;
        let _ = a.parse_nested_meta(|m| {
            if m.path.is_ident("C") {
                c = true;
            }
            Ok(())
        });
        c
    })
}

/// The Velt name of a field: `#[velt(name = "...")]`, else its name in camelCase.
fn velt_name(f: &syn::Field) -> syn::Result<String> {
    let mut name = None;
    for a in f.attrs.iter().filter(|a| a.path().is_ident("velt")) {
        a.parse_nested_meta(|m| {
            if m.path.is_ident("name") {
                name = Some(m.value()?.parse::<LitStr>()?.value());
                Ok(())
            } else {
                Err(m.error("unknown `velt` option (use `name = \"...\"`)"))
            }
        })?;
    }
    let ident = f.ident.as_ref().map(|i| i.to_string()).unwrap_or_default();
    Ok(name.unwrap_or_else(|| camel(&ident)))
}

pub fn expand(input: DeriveInput) -> syn::Result<Tokens> {
    let name = &input.ident;
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(
            input.generics.span(),
            "a Velt struct cannot be generic",
        ));
    }
    if !is_repr_c(&input) {
        return Err(syn::Error::new(
            name.span(),
            "a Velt struct needs `#[repr(C)]` (Velt lays fields out in order, as C does)",
        ));
    }
    let Data::Struct(s) = &input.data else {
        return Err(syn::Error::new(
            name.span(),
            "only a struct can be a Velt struct",
        ));
    };
    let Fields::Named(fields) = &s.fields else {
        return Err(syn::Error::new(
            name.span(),
            "a Velt struct has named fields",
        ));
    };
    if fields.named.is_empty() {
        return Err(syn::Error::new(
            name.span(),
            "a Velt struct needs at least one field",
        ));
    }
    let mut parts = vec![];
    for f in &fields.named {
        let n = velt_name(f)?;
        let ty = &f.ty;
        parts.push(quote! { (#n, <#ty as ::velt_native::Value>::SIG) });
    }
    Ok(quote! {
        // SAFETY: `#[repr(C)]` with fields that are Velt values, in Velt's order.
        unsafe impl ::velt_native::Value for #name {
            const SIG: ::velt_native::sig::Buf =
                ::velt_native::value::__record_sig(&[#(#parts),*]);
        }
        // SAFETY: as above.
        unsafe impl ::velt_native::Record for #name {}

        impl<'a> ::velt_native::Param<'a> for &'a #name {
            type Raw = *const #name;
            const SIG: ::velt_native::sig::Buf = <#name as ::velt_native::Value>::SIG;
            unsafe fn from_raw(raw: *const #name) -> &'a #name {
                &*raw
            }
        }

        impl ::velt_native::OutRet for #name {
            type Slot = #name;
            const SIG: ::velt_native::sig::Buf = <#name as ::velt_native::Value>::SIG;
            unsafe fn write(self, out: *mut #name) {
                out.write(self)
            }
            fn panicked(msg: &str) -> #name {
                ::velt_native::fatal(msg)
            }
            unsafe fn drop_slot(slot: *mut #name) {
                ::std::ptr::drop_in_place(slot)
            }
        }

        impl ::velt_native::ResultValue for #name {
            type Slot = #name;
            const SIG: ::velt_native::sig::Buf =
                ::velt_native::value::__io_result(<#name as ::velt_native::Value>::SIG);
            fn into_slot(self) -> #name {
                self
            }
            unsafe fn drop_value(slot: *mut #name) {
                ::std::ptr::drop_in_place(slot)
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_names_become_camel_case() {
        assert_eq!(camel("last_insert_rowid"), "lastInsertRowid");
        assert_eq!(camel("changes"), "changes");
        assert_eq!(camel("r#type"), "type");
    }

    fn err(input: DeriveInput) -> String {
        match expand(input) {
            Ok(_) => String::new(),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn needs_repr_c_named_fields_and_no_generics() {
        assert!(err(syn::parse_quote!(
            struct S {
                a: i64,
            }
        ))
        .contains("repr(C)"));
        assert!(err(syn::parse_quote!(
            #[repr(C)]
            struct S(i64);
        ))
        .contains("named fields"));
        assert!(err(syn::parse_quote!(
            #[repr(C)]
            struct S<T> {
                a: T,
            }
        ))
        .contains("generic"));
        assert!(err(syn::parse_quote!(
            #[repr(C)]
            struct S {}
        ))
        .contains("at least one"));
        assert_eq!(
            err(syn::parse_quote!(
                #[repr(C)]
                struct S {
                    a: i64,
                }
            )),
            ""
        );
    }
}
