//! The signature of a host function (`declare host("m") function`,
//! https://veltlang.com/docs/reference/host-functions/): what crosses between Velt and the JS host. Integers,
//! `f64`, `bool`, `string`, `u8[]` and `readonly u8[]`, std's `HostRef` and `HostRef | null`
//! cross, and `void` as the result; anything else is an error that names the parameter and
//! lists them. An `async` host function returns `Promise<T>` of such a result `T`: the promise
//! of the JS function, which rejects with an `IoError` (`Promise<T, IoError>`).

use velt_common::Diagnostic;
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::defs::ParamSig;
use crate::hir::{FloatTy, HostImport, HostTy, Nothing, TyId, TyKind};

/// std's handle to a JS value (std/host/host_ref.velt).
pub(crate) const HOST_REF: &str = velt_common::std_item!(host_host_ref, "HostRef");

/// What the promise of an async host function rejects with (loaded with the hooks that await it,
/// std/prelude/global/host_async.velt).
const IO_ERROR: &str = velt_common::std_item!(io_error, "IoError");

/// The import module the runtime's own imports come from (crates/velt_rt_wasm/js/velt_web.mjs):
/// a program's host functions cannot share it.
const RUNTIME_MODULE: &str = "velt";

/// Import modules named with this are the runtime glue's (`velt:worker`, `velt:fetch`): std
/// declares their functions once, so their types are the glue's, and only std may.
const GLUE_PREFIX: &str = "velt:";

/// What a parameter may be, for the errors.
const PARAMS_ALLOWED: &str = "integers, `f64`, `bool`, `string`, `u8[]`, `readonly u8[]` (a \
                              view of bytes at the call), `HostRef` and `HostRef | null`";

/// What a result may be, for the errors.
const RESULTS_ALLOWED: &str =
    "`void`, integers, `f64`, `bool`, `string`, `u8[]`, `HostRef` and `HostRef | null`";

/// The import of the host function `sig` (parameter types `ps`, result `ret`), reporting what
/// cannot cross. Reported parameters cross as `void`: the program does not compile. An async
/// one's `ret` becomes `Promise<T, IoError>`. `in_std`: the declaration is std's.
pub(super) fn host_import(
    cx: &mut Ctx,
    in_std: bool,
    sig: &ast::FnSig,
    ps: &[ParamSig],
    ret: &mut TyId,
) -> HostImport {
    let name = &sig.name.name;
    let module = sig
        .host
        .as_ref()
        .map_or_else(String::new, |h| h.module.clone());
    if let Some(h) = &sig.host {
        let why = if h.module.is_empty() {
            Some("a host function needs an import module: `declare host(\"<module>\")`")
        } else if h.module == RUNTIME_MODULE {
            Some("the import module `velt` is the runtime's own: choose another one")
        } else if h.module.starts_with(GLUE_PREFIX) && !in_std {
            Some(
                "the import modules `velt:…` are the runtime glue's: std declares their functions \
                 (import them from std, e.g. `velt/host/worker`)",
            )
        } else if h.module.chars().any(char::is_control) {
            Some("an import module cannot hold control characters")
        } else {
            None
        };
        if let Some(why) = why {
            cx.err(why, h.span);
        }
    }
    if let Some(g) = sig.generics.first() {
        cx.err(
            "a host function has no type parameters: every type that crosses is fixed",
            g.name.span,
        );
    }
    if sig.this_param.is_some() {
        cx.err("a host function has no `this` parameter", sig.name.span);
    }
    let ret_span = sig.ret.as_ref().map_or(sig.name.span, |t| t.span);
    if !sig.is_async && is_promise(cx, *ret) {
        cx.error(
            Diagnostic::error(
                format!("host function `{name}` returns a `Promise` but is not `async`"),
                ret_span,
            )
            .with_note(format!(
                "declare it `declare host(\"{module}\") async function {name}(…)`"
            )),
        );
    }
    let params = sig
        .params
        .iter()
        .zip(ps)
        .map(|(p, s)| param(cx, name, p, s.ty))
        .collect::<Vec<_>>();
    if sig.is_async {
        for (p, t) in sig.params.iter().zip(&params) {
            if *t == (HostTy::Bytes { readonly: false }) {
                cx.error(
                    Diagnostic::error(
                        format!("parameter `{}` of async host function `{name}` is a `u8[]` the host could write after the call", p.name.name),
                        p.ty.span,
                    )
                    .with_note("an async host function takes `readonly u8[]`: the host gets a copy"),
                );
            }
        }
    }
    let value = match cx.ty.promise_payload(*ret) {
        Some(v) if sig.is_async => {
            *ret = rejecting(cx, v, ret_span);
            v
        }
        _ => *ret,
    };
    let ret = match ret_ty(cx, value) {
        Some(t) => t,
        None => {
            // A `Promise` result without `async` is reported above.
            if sig.is_async || !is_promise(cx, value) {
                let shown = cx.display(value);
                let what = if sig.is_async {
                    "fulfills with"
                } else {
                    "returns"
                };
                cx.error(
                    Diagnostic::error(
                        format!("host function `{name}` {what} `{shown}`, which cannot cross to the host"),
                        ret_span,
                    )
                    .with_note(format!("a host function returns {RESULTS_ALLOWED}, or a `Promise` of one")),
                );
            }
            HostTy::Void
        }
    };
    HostImport {
        module,
        params,
        ret,
    }
}

/// What crosses for parameter `p` (type `ty`) of host function `name`.
fn param(cx: &mut Ctx, name: &str, p: &ast::Param, ty: TyId) -> HostTy {
    let what = if p.rest {
        Some("a rest parameter")
    } else if p.optional || p.default.is_some() {
        Some("an optional parameter")
    } else if p.pattern.is_some() {
        Some("a destructured parameter")
    } else {
        None
    };
    if let Some(what) = what {
        cx.err(
            format!("host function `{name}` cannot have {what}: the host gets every argument"),
            p.span,
        );
        return HostTy::Void;
    }
    let readonly = super::sigs::written_readonly(&p.ty);
    if let Some(t) = value_ty(cx, ty, readonly) {
        return t;
    }
    let shown = cx.display(ty);
    cx.error(
        Diagnostic::error(
            format!(
                "parameter `{}` of host function `{name}` has type `{shown}`, which cannot cross to the host",
                p.name.name
            ),
            p.ty.span,
        )
        .with_note(format!("a host function takes {PARAMS_ALLOWED}")),
    );
    HostTy::Void
}

/// What crosses for the result type `ty` (`None`: nothing can).
fn ret_ty(cx: &Ctx, ty: TyId) -> Option<HostTy> {
    match cx.ty.kind(ty) {
        TyKind::Unit => Some(HostTy::Void),
        // `readonly u8[]` is `u8[]` from here on: either crosses as new bytes.
        _ => value_ty(cx, ty, false),
    }
}

/// What crosses for a value of type `ty` (`readonly`: written `readonly u8[]`).
fn value_ty(cx: &Ctx, ty: TyId, readonly: bool) -> Option<HostTy> {
    match cx.ty.kind(ty) {
        TyKind::Int(i) => Some(HostTy::Int(*i)),
        TyKind::Float(FloatTy::F64) => Some(HostTy::F64),
        TyKind::Bool => Some(HostTy::Bool),
        TyKind::Str => Some(HostTy::Str),
        TyKind::Array(e) if matches!(cx.ty.kind(*e), TyKind::Int(crate::hir::IntTy::U8)) => {
            Some(HostTy::Bytes { readonly })
        }
        // `HostRef | null` (or `| undefined`): a null pointer is the nothing. Not
        // `HostRef | null | undefined`, whose two nothings need more than a pointer.
        TyKind::Option(inner, n) if *n != Nothing::Both && is_host_ref(cx, *inner) => {
            Some(HostTy::Ref { nullable: true })
        }
        _ if is_host_ref(cx, ty) => Some(HostTy::Ref { nullable: false }),
        _ => None,
    }
}

/// Is `ty` std's `HostRef`?
fn is_host_ref(cx: &Ctx, ty: TyId) -> bool {
    cx.class_of(ty)
        .and_then(|(d, _)| cx.adt(d))
        .is_some_and(|a| a.qual_name == HOST_REF)
}

/// `Promise<v, IoError>`, the type of an async host function's call.
fn rejecting(cx: &mut Ctx, v: TyId, span: velt_common::Span) -> TyId {
    let Some(crate::ctx::Item::Def(d)) = cx.std_def(IO_ERROR) else {
        cx.err(
            "an async host function needs `IoError` (velt/io), which std does not have",
            span,
        );
        return cx.ty.error;
    };
    let e = cx.ty.intern(TyKind::Adt(d, vec![]));
    cx.ty.promise_rejecting(v, e)
}

fn is_promise(cx: &Ctx, ty: TyId) -> bool {
    matches!(cx.ty.kind(ty), TyKind::Promise(..))
}

/// Every declaration of one host function (the same import module and name, in any modules)
/// must have the same types, and pass each the same way (`u8[]` and `readonly u8[]` are one
/// type but cross differently): the WebAssembly module imports it once.
pub(crate) fn same_imports(cx: &mut Ctx) {
    type Sig = (Vec<TyId>, TyId, Vec<HostTy>, HostTy);
    let mut seen: rustc_hash::FxHashMap<(String, String), Sig> = Default::default();
    let mut errors = vec![];
    for info in &cx.info {
        let crate::defs::DefInfo::Fn(f) = info else {
            continue;
        };
        let Some(h) = &f.host else { continue };
        let sig = (
            f.params.iter().map(|p| p.ty).collect::<Vec<_>>(),
            f.ret,
            h.params.clone(),
            h.ret,
        );
        let key = (h.module.clone(), f.name.clone());
        match seen.get(&key) {
            Some(first) if *first != sig => {
                let same_types = first.0 == sig.0 && first.1 == sig.1;
                errors.push((key, f.span, same_types));
            }
            Some(_) => {}
            None => {
                seen.insert(key, sig);
            }
        }
    }
    for ((module, name), span, same_types) in errors {
        let mut d = Diagnostic::error(
            format!("host function `{name}` of `{module}` is declared twice with different types"),
            span,
        )
        .with_note("the program imports it once: declare it once, and import that declaration");
        if same_types {
            d = d.with_note(
                "`u8[]` and `readonly u8[]` differ here: the host may write one of them",
            );
        }
        cx.error(d);
    }
}
