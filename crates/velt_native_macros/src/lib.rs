//! `#[velt_native::export]`: turns a Rust function into a C-ABI export of a Velt package's native
//! library, plus a `velt_sig_<name>` record of its signature (docs/internals/contracts/native_abi.md
//! "Signature records") that `velt native build` collects and the compiler checks every `declare`
//! against.
//!
//! - `#[export] fn f(a: &str, n: u64) -> R`: `declare function f(a: string, n: u64): R`. Scalar and
//!   `()` results are returned directly; `String`, `Vec<u8>` and `Result<T, Error>` go through a
//!   trailing out-pointer (rt_abi_async.md §3.1).
//! - `#[export(blocking)] fn f(a: String) -> Result<T, Error>`: `declare async function`; the body
//!   runs on the runtime's blocking pool (parameters must be owned).
//! - `#[export(future)] fn f(...) -> Future<R>`: `declare async function`; the body returns a
//!   runtime future (from a `Completer`).

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as Tokens};
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{parse_macro_input, FnArg, ItemFn, Pat, ReturnType, Type};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Sync,
    Blocking,
    Future,
}

const SCALARS: &[&str] = &[
    "bool", "u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64", "f32", "f64",
];

/// See the crate docs.
#[proc_macro_attribute]
pub fn export(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mode = match attr.to_string().trim() {
        "" => Mode::Sync,
        "blocking" => Mode::Blocking,
        "future" => Mode::Future,
        other => {
            return error(
                Span::call_site(),
                &format!("unknown `export` option `{other}` (use `blocking` or `future`)"),
            )
        }
    };
    let func = parse_macro_input!(item as ItemFn);
    match expand(mode, func) {
        Ok(t) => t.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn error(span: Span, msg: &str) -> TokenStream {
    syn::Error::new(span, msg).to_compile_error().into()
}

/// A reference with an explicit lifetime (`&'static str`, `&'a [u8]`), anywhere in `ty`: the
/// argument is borrowed only for the call, so naming a longer lifetime would let it dangle.
fn explicit_lifetime(ty: &Type) -> Option<&syn::TypeReference> {
    match ty {
        Type::Reference(r) if r.lifetime.is_some() => Some(r),
        Type::Reference(r) => explicit_lifetime(&r.elem),
        Type::Paren(p) => explicit_lifetime(&p.elem),
        Type::Group(g) => explicit_lifetime(&g.elem),
        _ => None,
    }
}

/// `ty` with every reference lifetime made `'static` (for naming `Param<'static>::Raw`).
fn staticize(ty: &Type) -> Type {
    match ty {
        Type::Reference(r) => {
            let mut r = r.clone();
            r.lifetime = Some(syn::Lifetime::new("'static", Span::call_site()));
            r.elem = Box::new(staticize(&r.elem));
            Type::Reference(r)
        }
        other => other.clone(),
    }
}

fn is_direct(ret: &ReturnType) -> bool {
    match ret {
        ReturnType::Default => true,
        ReturnType::Type(_, ty) => match &**ty {
            Type::Tuple(t) => t.elems.is_empty(),
            Type::Path(p) => p
                .path
                .get_ident()
                .is_some_and(|i| SCALARS.contains(&i.to_string().as_str())),
            _ => false,
        },
    }
}

fn expand(mode: Mode, func: ItemFn) -> syn::Result<Tokens> {
    let sig = &func.sig;
    if !sig.generics.params.is_empty() || sig.asyncness.is_some() || sig.variadic.is_some() {
        return Err(syn::Error::new(
            sig.span(),
            "an exported function cannot be generic, `async` or variadic (use `#[export(blocking)]`)",
        ));
    }
    let name = &sig.ident;
    let inner = format_ident!("__velt_inner_{}", name);
    let mut inner_fn = func.clone();
    inner_fn.sig.ident = inner.clone();
    inner_fn.attrs.retain(|a| !a.path().is_ident("no_mangle"));

    let mut raw_params = vec![];
    let mut args = vec![];
    let mut sig_parts = vec![];
    for (i, arg) in sig.inputs.iter().enumerate() {
        let FnArg::Typed(pt) = arg else {
            return Err(syn::Error::new(
                arg.span(),
                "an exported function cannot take `self`",
            ));
        };
        if !matches!(&*pt.pat, Pat::Ident(_) | Pat::Wild(_)) {
            return Err(syn::Error::new(pt.pat.span(), "use a plain parameter name"));
        }
        if let Some(r) = explicit_lifetime(&pt.ty) {
            return Err(syn::Error::new(
                r.span(),
                "write the parameter without a lifetime (`&str`, `&[u8]`): Velt lends it only for the call",
            ));
        }
        let ty = staticize(&pt.ty);
        let a = format_ident!("__a{}", i);
        raw_params.push(quote! { #a: <#ty as ::velt_native::Param<'static>>::Raw });
        args.push(a);
        if i > 0 {
            sig_parts.push(quote! { "," });
        }
        sig_parts.push(quote! { <#ty as ::velt_native::Param<'static>>::SIG });
    }
    let ret_ty: Type = match &sig.output {
        ReturnType::Default => syn::parse_quote!(()),
        ReturnType::Type(_, t) => (**t).clone(),
    };

    let catch = |call: Tokens, on_panic: Tokens| {
        quote! {
            match ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| #call)) {
                ::std::result::Result::Ok(r) => r,
                ::std::result::Result::Err(p) => #on_panic(&::velt_native::__panic_message(p)),
            }
        }
    };
    // `__scope` is a local of the wrapper: tying each borrowed argument to it makes a parameter
    // type that demands a longer borrow (`type S = &'static str;`) a compile error, however the
    // type is spelled. The syntactic lifetime check above only gives the common case a nicer
    // message.
    let from_raw = quote! { #(::velt_native::__from_raw_scoped(&__scope, #args)),* };

    let (prefix, ret_sig, wrapper) = match mode {
        Mode::Sync if is_direct(&sig.output) => {
            let body = catch(
                quote! { #inner(#from_raw) },
                quote! { <#ret_ty as ::velt_native::DirectRet>::panicked },
            );
            (
                "",
                quote! { <#ret_ty as ::velt_native::DirectRet>::SIG },
                quote! {
                    #[no_mangle]
                    pub unsafe extern "C" fn #name(#(#raw_params),*) -> #ret_ty {
                        #inner_fn
                        let __scope = ();
                        #body
                    }
                },
            )
        }
        Mode::Sync => {
            let body = catch(
                quote! { #inner(#from_raw) },
                quote! { <#ret_ty as ::velt_native::OutRet>::panicked },
            );
            (
                "",
                quote! { <#ret_ty as ::velt_native::OutRet>::SIG },
                quote! {
                    #[no_mangle]
                    pub unsafe extern "C" fn #name(
                        #(#raw_params,)*
                        __out: *mut <#ret_ty as ::velt_native::OutRet>::Slot,
                    ) {
                        #inner_fn
                        let __scope = ();
                        let __r: #ret_ty = #body;
                        ::velt_native::OutRet::write(__r, __out)
                    }
                },
            )
        }
        Mode::Blocking => {
            let owned: Vec<Tokens> = sig
                .inputs
                .iter()
                .zip(&args)
                .map(|(arg, a)| {
                    let FnArg::Typed(pt) = arg else { unreachable!() };
                    let ty = &pt.ty;
                    quote! { let #a: #ty = ::velt_native::__owned(::velt_native::Param::from_raw(#a)); }
                })
                .collect();
            (
                "async ",
                quote! { <#ret_ty as ::velt_native::OutRet>::SIG },
                quote! {
                    #[no_mangle]
                    pub unsafe extern "C" fn #name(#(#raw_params),*) -> *mut ::std::ffi::c_void {
                        #inner_fn
                        #(#owned)*
                        ::velt_native::__blocking::<#ret_ty, _>(move || #inner(#(#args),*))
                    }
                },
            )
        }
        Mode::Future => {
            let body = catch(
                quote! { #inner(#from_raw) },
                quote! { ::velt_native::fatal },
            );
            (
                "async ",
                quote! { <#ret_ty as ::velt_native::FutureRet>::SIG },
                quote! {
                    #[no_mangle]
                    pub unsafe extern "C" fn #name(#(#raw_params),*) -> *mut ::std::ffi::c_void {
                        #inner_fn
                        let __scope = ();
                        let __f: #ret_ty = #body;
                        ::velt_native::FutureRet::__into_raw(__f)
                    }
                },
            )
        }
    };

    let sig_static = format_ident!("__VELT_SIG_{}", name);
    let sig_symbol = format!("velt_sig_{name}");
    let open = format!("{prefix}(");
    let parts = quote! { &[#open, #(#sig_parts,)* ")->", #ret_sig] };
    Ok(quote! {
        #wrapper

        #[used]
        #[doc(hidden)]
        #[allow(non_upper_case_globals)]
        #[export_name = #sig_symbol]
        pub static #sig_static: [u8; ::velt_native::sig::len(#parts)] =
            ::velt_native::sig::build(#parts);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(f: ItemFn) -> String {
        match expand(Mode::Sync, f) {
            Ok(_) => String::new(),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn rejects_explicit_lifetimes_on_borrowed_parameters() {
        let e = err(syn::parse_quote!(
            fn velt_p_f(s: &'static str) {}
        ));
        assert!(e.contains("without a lifetime"), "{e}");
        let e = err(syn::parse_quote!(
            fn velt_p_f<'a>(b: &'a [u8]) {}
        ));
        assert!(!e.is_empty(), "generic lifetime accepted");
        assert_eq!(
            err(syn::parse_quote!(
                fn velt_p_f(s: &str, b: &[u8]) {}
            )),
            ""
        );
    }
}
