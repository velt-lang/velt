//! The signature of a C function (`declare c("lib") function`, docs/internals/design/c-ffi.md):
//! what crosses to C. Integers, `f32`, `f64` and `bool` cross by value, and `void` as the
//! result; pointers, buffers, strings and structs come with `velt/ffi`. Anything else is an
//! error naming the parameter. Host functions' signatures are made here too (`host_sig`).

use velt_common::Diagnostic;
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::defs::{FnKind, ParamSig};
use crate::hir::{CImport, CTy, DefId, FloatTy, TyId, TyKind};

/// What may cross, for the errors.
const ALLOWED: &str = "integers (`i8` … `u64`, `isize`, `usize`), `f32`, `f64` and `bool`";

/// The imports of extern function `d` (`kind`, declared in `module`) with signature `sig`:
/// a host function's (`host_sig`) or a C function's, recorded on its `FnInfo`.
pub(super) fn foreign_imports(
    cx: &mut Ctx,
    (d, kind, module): (DefId, FnKind, usize),
    sig: &ast::FnSig,
    ps: &[ParamSig],
    ret: &mut TyId,
) {
    if kind != FnKind::Extern {
        return;
    }
    let host = sig
        .host
        .is_some()
        .then(|| super::host_sig::host_import(cx, cx.scopes[module].is_std, sig, ps, ret));
    let c = sig
        .c
        .as_ref()
        .map(|lib| c_import(cx, &lib.library, lib.span, sig, ps, *ret));
    let f = cx.fn_info_mut(d);
    f.host = host;
    f.c = c;
}

/// The import of C function `sig` of `library` (parameter types `ps`, result `ret`), reporting
/// what cannot cross. Reported types cross as `void`: the program does not compile.
fn c_import(
    cx: &mut Ctx,
    library: &str,
    at: velt_common::Span,
    sig: &ast::FnSig,
    ps: &[ParamSig],
    ret: TyId,
) -> CImport {
    let name = &sig.name.name;
    if library.is_empty()
        || library
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        cx.err(
            "a C library's name is a plain logical name (`declare c(\"sqlite3\")`), which the package's manifest maps to a library per target",
            at,
        );
    }
    let fixed = |what: &str| format!("a C function {what}: its C signature fixes every type");
    if let Some(g) = sig.generics.first() {
        cx.err(fixed("has no type parameters"), g.name.span);
    }
    if sig.this_param.is_some() {
        cx.err(fixed("has no `this` parameter"), sig.name.span);
    }
    if sig.is_async {
        cx.error(
            Diagnostic::error(
                format!("C function `{name}` can't be `async` yet"),
                sig.name.span,
            )
            .with_note("async C calls (on the library's pool, or with a `Completion`) come in a later step of #2187"),
        );
    }
    let params = sig
        .params
        .iter()
        .zip(ps)
        .map(|(p, s)| param(cx, name, p, s.ty))
        .collect();
    let ret_ty = match cx.ty.kind(ret) {
        TyKind::Unit => Some(CTy::Void),
        _ => value_ty(cx, ret),
    };
    let ret = ret_ty.unwrap_or_else(|| {
        let shown = cx.display(ret);
        let span = sig.ret.as_ref().map_or(sig.name.span, |t| t.span);
        cx.error(
            Diagnostic::error(
                format!("C function `{name}` returns `{shown}`, which cannot cross from C"),
                span,
            )
            .with_note(format!("a C function returns `void`, {ALLOWED}")),
        );
        CTy::Void
    });
    CImport {
        library: library.to_string(),
        params,
        ret,
    }
}

/// What crosses for parameter `p` (type `ty`) of C function `name`.
fn param(cx: &mut Ctx, name: &str, p: &ast::Param, ty: TyId) -> CTy {
    let what = if p.rest {
        Some("a rest parameter (C's `...` is declared `...args: CVarArgs`, coming later)")
    } else if p.optional || p.default.is_some() {
        Some("an optional parameter")
    } else if p.pattern.is_some() {
        Some("a destructured parameter")
    } else {
        None
    };
    if let Some(what) = what {
        cx.err(
            format!("C function `{name}` cannot have {what}: C takes every argument"),
            p.span,
        );
        return CTy::Void;
    }
    if let Some(t) = value_ty(cx, ty) {
        return t;
    }
    let shown = cx.display(ty);
    cx.error(
        Diagnostic::error(
            format!(
                "parameter `{}` of C function `{name}` has type `{shown}`, which cannot cross to C",
                p.name.name
            ),
            p.ty.span,
        )
        .with_note(format!("a C function takes {ALLOWED}")),
    );
    CTy::Void
}

/// What crosses for a value of type `ty` (`None`: nothing can).
fn value_ty(cx: &Ctx, ty: TyId) -> Option<CTy> {
    match cx.ty.kind(ty) {
        TyKind::Int(i) => Some(CTy::Int(*i)),
        TyKind::Float(FloatTy::F32) => Some(CTy::F32),
        TyKind::Float(FloatTy::F64) => Some(CTy::F64),
        TyKind::Bool => Some(CTy::Bool),
        _ => None,
    }
}
