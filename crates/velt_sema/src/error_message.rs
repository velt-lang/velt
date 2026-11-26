//! `get message()` on a class extending `Error` (#3792), as JavaScript has it: `Error`'s
//! constructor gives an instance its own `message` only when it is passed one, and reading
//! `message` finds that own value first, else `Error.prototype.message`, which the class's getter
//! replaces. So `new E("given").message` is `"given"` and `new E().message` is what the getter
//! returns.
//!
//! In Velt `message` is a field of `Error`. In a program with such a getter (and only there, so
//! every other program compiles exactly as before):
//!
//! - the getter is the override of a std-only accessor of `Error`, keyed by `ERROR_MESSAGE`
//!   (std/lang/hooks/keys.velt; its base returns `""`, `Error.prototype.message`):
//!   [`mark_getters`] notes its name before signatures are collected, and `Ctx::method_key`
//!   gives that name the key, so every table and override sees one member;
//! - the constructors' default message `noMessage()` (std/lang/hooks/error.velt) stays a call,
//!   whose empty string at an address of its own means "no own message"; elsewhere a placed
//!   default of it is the literal `""` ((`body::expr::error_message`));
//! - a read of `Error`'s `message` (in the program and in std: `toString`, `stack`, the report of
//!   an uncaught error) is a call of the hook `errorMessage(e)`, the own message unless it is that
//!   empty string, else the accessor, dispatched ; `super.message`
//!   in the getter is the base's accessor ;
//! - a write stays a field write. Through a type that has the getter it is a compile error, as in
//!   TypeScript (a property with only a getter is read-only, TS2540). Through `Error` or a class
//!   without the getter it writes the own message; Node throws a `TypeError` there when the error
//!   has no own message yet, which a plain assignment can't throw in Velt (it would be an error
//!   every `e.message = …` of the program has to declare).
//!
//! A `set message()` is not supported yet ([`refuse_setter`]). A check by units
//! (`VELT_UNITS`) checks std's bodies before the program's classes are known, so it refuses such
//! a getter rather than give std's reads another answer.

use velt_common::{std_key, Diagnostic};

use crate::ctx::Ctx;
use crate::hir::{DefId, TyId};

/// The member name of `Error`'s accessor the getters override.
pub(crate) const KEY: &str = std_key!(error_message);

/// Notes the name of every `get message()` of a class from definition `from` on that extends the
/// prelude's `Error` (`KeyNames::error_message_getters`), before their signatures are collected.
pub(crate) fn mark_getters(cx: &mut Ctx, from: u32) {
    let Some(error) = cx.prelude_adt("Error") else {
        return;
    };
    for i in from..cx.info.len() as u32 {
        let d = DefId(i);
        let Some(decl) = cx.adt(d).and_then(|a| a.decl) else {
            continue;
        };
        if d == error || !cx.class_extends(d, error) {
            continue;
        }
        let getters = (decl.methods.iter())
            .filter(|m| m.is_getter && !m.is_static && m.decl.sig.name.name == "message")
            .map(|m| m.decl.sig.name.span);
        for span in getters.collect::<Vec<_>>() {
            if cx.units {
                cx.error(
                    Diagnostic::error(
                        "`get message()` on a class extending `Error` is not supported in a check by units (`VELT_UNITS`)",
                        span,
                    )
                    .with_note("std's code is checked before the program's classes are known, so it would read the error's own `message` where JavaScript calls this getter")
                    .with_note("build without `VELT_UNITS`"),
                );
            }
            // Marked in a check by units too, so the refusal is its only error.
            cx.symbol_keys.mark_error_message_getter(span);
        }
    }
}

/// Reports `set message()` (method `m`) of class `d` when `d` extends `Error`: not supported yet.
pub(crate) fn refuse_setter(cx: &mut Ctx, d: DefId, m: &velt_syntax::ast::Method) -> bool {
    let error = cx.prelude_adt("Error");
    let extends = error.is_some_and(|e| e != d && cx.class_extends(d, e));
    if !extends || !m.is_setter || m.is_static || m.decl.sig.name.name != "message" {
        return false;
    }
    cx.error(
        Diagnostic::error(
            "`set message()` on a class extending `Error` is not supported yet",
            m.decl.sig.name.span,
        )
        .with_note("`get message()` alone is: it gives the errors made without a message theirs"),
    );
    true
}

/// Does the program declare such a getter ([`mark_getters`])?
pub(crate) fn on(cx: &Ctx) -> bool {
    cx.symbol_keys.has_error_message_getters()
}

/// Is `t` the prelude's `Error` or a class extending it?
pub(crate) fn error_class(cx: &Ctx, t: TyId) -> Option<DefId> {
    let error = cx.prelude_adt("Error")?;
    let (d, _) = cx.class_of(t)?;
    cx.class_extends(d, error).then_some(error)
}

/// Does class type `t` (a class extending `Error`) or one of its bases declare the getter?
pub(crate) fn has_getter(cx: &mut Ctx, t: TyId) -> bool {
    let Some((d, args)) = cx.class_of(t) else {
        return false;
    };
    let error = cx.prelude_adt("Error");
    crate::collect::lookup_method(cx, d, &args, KEY)
        .and_then(|f| match f {
            crate::collect::Found::Class { owner, .. } => Some(owner),
            _ => None,
        })
        .is_some_and(|owner| Some(owner) != error)
}

/// Reports each such getter that can throw, once errors are inferred: every read of an error's
/// `message` calls it, including those of `String(e)`, `console.log` and the report of an uncaught
/// error, where its error would have nowhere to go (a `toString()` that throws is refused alike,
/// `hooks::check_throws`).
pub(crate) fn check_throws(cx: &mut Ctx) {
    if !on(cx) {
        return;
    }
    let error = cx.prelude_adt("Error");
    for i in 0..cx.info.len() as u32 {
        let d = DefId(i);
        if Some(d) == error || cx.adt(d).is_none_or(|a| a.decl.is_none()) {
            continue;
        }
        let Some(m) = cx.adt(d).and_then(|a| a.methods.get(KEY)).map(|r| r.def) else {
            continue;
        };
        let Some(t) = cx.fn_throws(m).filter(|&t| t != cx.ty.never) else {
            continue;
        };
        let (span, tn) = (cx.fn_info(m).name_span, cx.display(t));
        cx.error(
            Diagnostic::error(format!("`get message()` of an error class cannot throw (it throws `{tn}`)"), span)
                .with_note("`String(e)`, `console.log` and the report of an uncaught error read `message`, and the error would have nowhere to go")
                .with_note("the getters of `message` are called through one entry, so they share one error type: one that throws makes each of them throw")
                .with_note("catch the error inside the getter and return a message instead"),
        );
    }
}

/// The value of the getter slot `<message>` of class type `self_ty` (`this` is local 0), in a
/// program with the getter, when `self_ty` extends `Error`: `errorMessage(this)`, as a typed
/// read is. A read through a bound or an interface (`T extends { message: string }`) calls the
/// slot, so it gets the getter's text too. `None` otherwise (the field, as before).
pub(crate) fn slot_read(
    cx: &mut Ctx,
    self_ty: TyId,
    fname: &str,
    this: &crate::hir::Expr,
    span: velt_common::Span,
) -> Option<crate::hir::Expr> {
    use crate::hir::{Expr, ExprKind as H, TyKind};
    use crate::hook_calls::{self as hooks, ty, HookTy};
    if fname != "message" || !on(cx) {
        return None;
    }
    let error = error_class(cx, self_ty)?;
    let error_ty = cx.ty.intern(TyKind::Adt(error, vec![]));
    let e = match self_ty == error_ty {
        true => this.clone(),
        false => Expr {
            kind: H::Upcast(Box::new(this.clone())),
            ty: error_ty,
            span,
        },
    };
    let str_ = cx.ty.str_;
    hooks::error_message(ty::Error::of(e)).plain(cx, str_, span)
}
