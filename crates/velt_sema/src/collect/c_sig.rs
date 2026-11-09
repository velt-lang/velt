//! The signature of a C function (`declare c("lib") function`, docs/internals/design/c-ffi.md):
//! what crosses to C. Integers, `f32`, `f64` and `bool` cross by value, and `void` as the
//! result; pointers, buffers, strings and structs come with `velt/ffi`. Anything else is an
//! error naming the parameter. Host functions' signatures are made here too (`host_sig`).

use velt_common::Diagnostic;
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::defs::{FnKind, ParamSig};
use crate::hir::{CImport, CTy, DefId, FloatTy, Nothing, PassMode, TyId, TyKind};

/// What may cross, for the errors.
const ALLOWED: &str = "integers (`i8` … `u64`, `isize`, `usize`), `f32`, `f64`, `bool`, \
                       `Ptr<T>` and `Ptr<T> | null` (velt/ffi), and for a parameter `null`, \
                       `StrBytes`, `CString`, `Out<T>` of a number and `OutPtr<T>` (velt/ffi), and arrays of numbers (`u8[]`, `readonly f64[]`)";

/// The module of std's `StrBytes` (std/ffi/str_bytes.velt).
const STR_BYTES_MODULE: &str = velt_common::std_module!(ffi_str_bytes);

/// velt/ffi's classes a C function takes by address (std/ffi/{c_string,out,out_ptr}.velt).
const C_STRING: &str = velt_common::std_item!(ffi_c_string, "CString");
const OUT: &str = velt_common::std_item!(ffi_out, "Out");
const OUT_PTR: &str = velt_common::std_item!(ffi_out_ptr, "OutPtr");

/// std's pointer into C memory (std/ffi/ptr.velt).
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
    let c = sig.c.as_ref().map(|lib| CImport {
        errno: lib.errno,
        ..c_import(cx, module, (&lib.library, lib.span), sig, ps, *ret)
    });
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
        if cx.is_c_struct_ty(ret) {
            struct_by_value(cx, name, &shown, span);
            return CTy::Void;
        }
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
        errno: false,
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
    if let Some(t) = by_address(cx, ty) {
        return t;
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
    if cx.is_c_struct_ty(ty) {
        struct_by_value(cx, name, &shown, p.ty.span);
        return CTy::Void;
    }
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

/// The error for C struct `shown` passed to or returned from C function `name` by value, which
/// waits for the target's C calling convention in both backends (#2187, layer 3 of C structs).
fn struct_by_value(cx: &mut Ctx, name: &str, shown: &str, at: velt_common::Span) {
    cx.error(
        Diagnostic::error(
            format!("C function `{name}` can't take or return C struct `{shown}` by value yet"),
            at,
        )
        .with_note("passing C structs by value, in each target's C calling convention, comes in a later layer of #2187; a `Ptr<S>` crosses now"),
    );
}

impl Ctx<'_> {
    /// The initializer the compiler makes for global `d` (of type `ann`), if any: a symbol
    /// constant's or a C constant's.
    pub(crate) fn made_init(&mut self, d: DefId, ann: Option<TyId>) -> Option<crate::hir::Expr> {
        self.symbol_init(d, ann)
            .or_else(|| self.c_const_init(d, ann))
    }

    /// The initializer of global `d` (of type `ann`) when it is a `declare c const`: a
    /// placeholder never evaluated, as its value comes from C headers after sema
    /// (`GlobalDef::c_const`) and lowering reads that. A type that can't hold one is reported.
    fn c_const_init(&mut self, d: DefId, ann: Option<TyId>) -> Option<crate::hir::Expr> {
        let g = self.global(d).filter(|g| g.src.c_const)?;
        let (name, at) = (g.name.clone(), g.src.span);
        let t = ann?;
        let cty = value_ty(self, t);
        if cty.is_none() && t != self.ty.error {
            let shown = self.display(t);
            self.error(
                Diagnostic::error(
                    format!("C constant `{name}` has type `{shown}`, which can't hold a C constant"),
                    at,
                )
                .with_note("a C constant is an integer (`i8` … `u64`, `isize`, `usize`), `f32`, `f64`, `bool`, `Ptr<T>` or `Ptr<T> | null` (velt/ffi)"),
            );
        }
        if let crate::defs::DefInfo::Global(g) = &mut self.info[d.0 as usize] {
            g.c_const = cty;
        }
        Some(crate::hir::Expr {
            kind: crate::hir::ExprKind::Lit(crate::hir::Lit::Unit),
            ty: t,
            span: at,
        })
    }
}

/// What crosses for a value of type `ty` (`None`: nothing can).
fn value_ty(cx: &Ctx, ty: TyId) -> Option<CTy> {
    match cx.ty.kind(ty) {
        TyKind::Int(i) => Some(CTy::Int(*i)),
        TyKind::Float(FloatTy::F32) => Some(CTy::F32),
        TyKind::Float(FloatTy::F64) => Some(CTy::F64),
        TyKind::Bool => Some(CTy::Bool),
        // `Ptr<T> | null` (or `| undefined`): `NULL` is the nothing.
        TyKind::Option(inner, n) if *n != Nothing::Both && is_c_ptr(cx, *inner) => {
            Some(CTy::Ptr { nullable: true })
        }
        _ if is_c_ptr(cx, ty) => Some(CTy::Ptr { nullable: false }),
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

/// velt/ffi's `CString`, `Out<T>` (of a number or `bool`) or `OutPtr<T>`, which cross as an
/// address.
fn by_address(cx: &Ctx, ty: TyId) -> Option<CTy> {
    let TyKind::Adt(d, args) = cx.ty.kind(ty) else {
        return None;
    };
    let scalar = |t: &TyId| is_number(cx, *t) || matches!(cx.ty.kind(*t), TyKind::Bool);
    match cx.adt(*d)?.qual_name.as_str() {
        C_STRING => Some(CTy::CString),
        OUT if args.first().is_some_and(scalar) => Some(CTy::Out),
        OUT_PTR if args.first().is_some_and(|t| c_pointee(cx, *t)) => Some(CTy::OutPtr),
        _ => None,
    }
}

/// Is `ty` a `Ptr<T>` C can hold: one to a C type (`declare c type`, `declare c struct`), a
/// number, `bool`, `void` or another such pointer (`Ptr<Ptr<u8>>` for `char **`)? `Ptr<string>`
/// means nothing in C.
pub(super) fn is_c_ptr(cx: &Ctx, ty: TyId) -> bool {
    match cx.ty.kind(ty) {
        TyKind::Adt(_, args) if is_ptr(cx, ty) => args.first().is_some_and(|t| c_pointee(cx, *t)),
        _ => false,
    }
}

/// Can a C pointer point at a `ty`?
fn c_pointee(cx: &Ctx, ty: TyId) -> bool {
    match cx.ty.kind(ty) {
        TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Unit => true,
        TyKind::Adt(d, _) => {
            cx.adt(*d)
                .is_some_and(|a| a.decl.is_some_and(|t| t.c_type || t.c_struct))
                || is_c_ptr(cx, ty)
        }
        _ => false,
    }
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

/// A `struct` an extern takes is the caller's storage, never a copy of it (#641): the runtime or
/// the C function fills a runtime layout through it (a system function writes one not written
/// `readonly`, in `fill_sig`; a runtime function one it names `out` or `slot`).
///
/// A runtime function std declares writes the `string` it takes as `out` or `buf`: a string
/// builder it appends to, or the slot it puts its result in (https://veltlang.com/docs/reference/modules/
/// "Runtime declarations"). Its argument is a mutable borrow, so the owner of the place is
/// modified too: a `StringBuilder` param of a function that appends to it is no read-only
/// (`frozen`) one whose fields the optimizer may read once.
pub(super) fn extern_struct_params(
    cx: &Ctx,
    (kind, module): (FnKind, usize),
    sig: &ast::FnSig,
    intrinsic: bool,
    ps: &mut [ParamSig],
) {
    if kind != FnKind::Extern || intrinsic {
        return;
    }
    if !sig.is_system && cx.scopes[module].is_std {
        for p in &mut *ps {
            let written = p.name == "out" || p.name == "buf";
            if written && p.mode == PassMode::Borrow && matches!(cx.ty.kind(p.ty), TyKind::Str) {
                p.mode = PassMode::BorrowMut;
            }
        }
    }
    for p in ps {
        if p.mode == PassMode::Copy && cx.declared_struct(p.ty) {
            let written = !sig.is_system && (p.name == "out" || p.name == "slot");
            p.mode = if written {
                PassMode::BorrowMut
            } else {
                PassMode::Borrow
            };
        }
    }
}
