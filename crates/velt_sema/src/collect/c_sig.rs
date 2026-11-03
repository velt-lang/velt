//! The signature of a C function (`declare c("lib") function`, docs/internals/design/c-ffi.md):
//! what crosses to C. Integers, `f32`, `f64` and `bool` cross by value, and `void` as the
//! result; pointers, buffers, strings and structs come with `velt/ffi`. Anything else is an
//! error naming the parameter. Host functions' signatures are made here too (`host_sig`).

use velt_common::Diagnostic;
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::defs::{FnKind, ParamSig};
use crate::hir::{CImport, CTy, DefId, FloatTy, Nothing, TyId, TyKind};

/// What may cross, for the errors.
const ALLOWED: &str = "integers (`i8` … `u64`, `isize`, `usize`), `f32`, `f64`, `bool`, \
                       `Ptr<T>` and `Ptr<T> | null` (velt/ffi), and for a parameter `null`, \
                       `StrBytes` (velt/ffi) and arrays of numbers (`u8[]`, `readonly f64[]`)";

/// The module of std's `StrBytes` (std/ffi/str_bytes.vlt).
const STR_BYTES_MODULE: &str = velt_common::std_module!(ffi_str_bytes);

/// std's pointer into C memory (std/ffi/ptr.vlt).
const PTR: &str = velt_common::std_item!(ffi_ptr, "Ptr");

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
        .map(|lib| c_import(cx, module, (&lib.library, lib.span), sig, ps, *ret));
    let f = cx.fn_info_mut(d);
    f.host = host;
    f.c = c;
}

/// The import of C function `sig` of `library` (parameter types `ps`, result `ret`), reporting
/// what cannot cross. Reported types cross as `void`: the program does not compile.
fn c_import(
    cx: &mut Ctx,
    module: usize,
    (library, at): (&str, velt_common::Span),
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
        .map(|(p, s)| param(cx, module, name, p, s.ty))
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
fn param(cx: &mut Ctx, module: usize, name: &str, p: &ast::Param, ty: TyId) -> CTy {
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
    if is_null(cx, ty) {
        return CTy::Null;
    }
    match cx.ty.kind(ty) {
        TyKind::Str if is_str_bytes(cx, module, &p.ty) => return CTy::StrBytes,
        TyKind::Str => {
            cx.error(
                Diagnostic::error(
                    format!(
                        "`string` cannot be passed to C: declare parameter `{}` of `{name}` as `StrBytes` (velt/ffi) to pass its bytes, borrowed",
                        p.name.name
                    ),
                    p.ty.span,
                )
                .with_note("`StrBytes` passes the string's UTF-8 bytes without a NUL, and the binding passes their length (`byteLength(s)`); a C function that needs a NUL-terminated string takes a `CString`"),
            );
            return CTy::Void;
        }
        TyKind::Array(e) if is_number(cx, *e) => {
            return CTy::Buf {
                readonly: super::sigs::written_readonly(&p.ty),
            };
        }
        _ => {}
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
        // `Ptr<T> | null` (or `| undefined`): `NULL` is the nothing.
        TyKind::Option(inner, n) if *n != Nothing::Both && is_ptr(cx, *inner) => {
            Some(CTy::Ptr { nullable: true })
        }
        _ if is_ptr(cx, ty) => Some(CTy::Ptr { nullable: false }),
        _ => None,
    }
}

/// Does the written type `t` (in `module`) name velt/ffi's `StrBytes`, directly
/// (`StrBytes`, imported) or through a namespace (`ffi.StrBytes`)? A look-alike alias of
/// `string` does not.
fn is_str_bytes(cx: &Ctx, module: usize, t: &ast::TypeExpr) -> bool {
    let ast::TypeExprKind::Named { path, args } = &t.kind else {
        return false;
    };
    if !args.is_empty() {
        return false;
    }
    let item = match path.as_slice() {
        [n] => cx.scopes[module].items.get(&n.name).copied(),
        // `ffi.StrBytes`: what the namespace's module exports under the name.
        [ns, n] => match cx.scopes[module].namespaces.get(&ns.name) {
            Some(&m) => super::exports::export_of(cx, m, &n.name),
            None => return false,
        },
        _ => return false,
    };
    // By the alias's own declaration, so a renamed import (`StrBytes as SB`) is it too.
    match item {
        Some(crate::ctx::Item::Alias(a)) => {
            let alias = &cx.aliases[a as usize];
            let m = alias.module;
            alias.decl.name.name == "StrBytes"
                && cx.modules[m].is_std
                && cx.modules[m].path == STR_BYTES_MODULE
        }
        _ => false,
    }
}

/// Is `ty` a number type an array of which C reads as a C array (integers, `f32`, `f64`)?
fn is_number(cx: &Ctx, ty: TyId) -> bool {
    matches!(
        cx.ty.kind(ty),
        TyKind::Int(_) | TyKind::Float(FloatTy::F32 | FloatTy::F64)
    )
}

/// The error for using a `Ptr<A>` where a `Ptr<B>` is expected (`expected`; `from` the value's
/// type), or `None` when `expected` is no `Ptr`: pointers to different C types never convert
/// implicitly, so the messages of other types' conversions don't fit.
pub(crate) fn ptr_conversion(
    cx: &Ctx,
    from: TyId,
    expected: TyId,
    at: velt_common::Span,
) -> Option<Diagnostic> {
    if !is_ptr(cx, expected) {
        return None;
    }
    let (f, e) = (cx.display(from), cx.display(expected));
    let target = match cx.ty.kind(expected) {
        TyKind::Adt(_, args) if args.len() == 1 => cx.display(args[0]),
        _ => "T".into(),
    };
    Some(
        Diagnostic::error(format!("a `{f}` is not a `{e}`"), at).with_note(format!(
            "pointers to different C types never convert implicitly: in `unsafe` code, `Ptr.at<{target}>(Ptr.addressOf(p))` makes one, stating that a `{target}` is at that address"
        )),
    )
}

/// Is `ty` std's `Ptr<T>`?
fn is_ptr(cx: &Ctx, ty: TyId) -> bool {
    match cx.ty.kind(ty) {
        TyKind::Adt(d, _) => cx.adt(*d).is_some_and(|a| a.qual_name == PTR),
        _ => false,
    }
}

/// Is `ty` the type `null` (an option of nothing)?
fn is_null(cx: &Ctx, ty: TyId) -> bool {
    matches!(cx.ty.kind(ty), TyKind::Option(inner, Nothing::Null) if *inner == cx.ty.never)
}
