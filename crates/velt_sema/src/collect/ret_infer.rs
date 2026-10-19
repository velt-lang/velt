//! Functions and methods without a return type take it from their body, as in TypeScript
//! (docs/reference/functions.md "Return types"). Collection decides which ones do: a body with
//! a `return` that has a value (any other body returns `void`, known right away). Dispatch needs
//! signatures before bodies are checked, so an unannotated method that overrides a base method
//! or implements an interface method takes that method's result type (the body is checked
//! against it); a comparison with a result still being inferred waits until the bodies are
//! checked (`crate::body::returns`).

use velt_common::Span;
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::defs::{FnSource, RetCheck, RetSource, RetWant};
use crate::hir::{DefId, TyId};

/// Does `source` have a `return` with a value (outside nested functions and arrows)?
pub(super) fn returns_value(source: Option<FnSource>) -> bool {
    match source {
        Some(FnSource::Decl(d)) => d.body.stmts.iter().any(stmt_returns_value),
        Some(FnSource::Default(_, b)) => b.stmts.iter().any(stmt_returns_value),
        None => false,
    }
}

pub(crate) fn stmt_returns_value(s: &ast::Stmt) -> bool {
    use ast::StmtKind as K;
    let block = |b: &ast::Block| b.stmts.iter().any(stmt_returns_value);
    match &s.kind {
        K::Return(e) => e.is_some(),
        K::If { then, els, .. } => block(then) || els.as_deref().is_some_and(stmt_returns_value),
        K::While { body, .. } | K::DoWhile { body, .. } | K::ForOf { body, .. } => block(body),
        K::For { body, .. } | K::Block(body) => block(body),
        K::Labeled { body, .. } => stmt_returns_value(body),
        K::Switch { cases, .. } => cases.iter().any(|c| c.body.iter().any(stmt_returns_value)),
        K::Try {
            body,
            catch,
            finally,
        } => {
            block(body)
                || catch.as_ref().is_some_and(|(_, b)| block(b))
                || finally.as_ref().is_some_and(block)
        }
        K::Var(_) | K::Expr(_) | K::Break(_) | K::Continue(_) | K::Throw(_) => false,
        K::Item(_) | K::Empty => false,
    }
}

/// Is `d`'s result type still to be inferred?
pub(super) fn is_pending(cx: &Ctx, d: DefId) -> bool {
    cx.fn_info(d).ret_source != RetSource::Known
}

/// The result of override `m` against `base`'s (substituted with `args`, which gives `want`):
/// an unannotated override takes it, a pending base is compared later. Whether the result
/// types agree as far as known now.
pub(super) fn override_ret(
    cx: &mut Ctx,
    (m, base): (DefId, DefId),
    args: &[TyId],
    (have, want): (TyId, TyId),
    (span, message): (Span, String),
) -> bool {
    cx.overridden.insert(base);
    if cx.fn_info(m).ret_source == RetSource::Body {
        cx.fn_info_mut(m).ret_source = RetSource::Base(base, args.to_vec());
        return true;
    }
    if is_pending(cx, base) {
        cx.ret_checks.push(RetCheck {
            def: m,
            args: vec![],
            want: RetWant::Of(base, args.to_vec()),
            span,
            message,
        });
        return true;
    }
    have == want
}

/// The result of method `def` implementing an interface method whose result is `want` (in
/// the implementing type's context, which `owner_args` maps `def`'s own context to): an
/// unannotated method of the implementing type itself takes it, a pending one is compared
/// later. Whether the result types agree as far as known now.
pub(super) fn impl_ret(
    cx: &mut Ctx,
    (def, owner_args): (DefId, &[TyId]),
    want: TyId,
    (span, message): (Span, String),
) -> bool {
    if !is_pending(cx, def) {
        let have = cx.fn_info(def).ret;
        return cx.subst(have, owner_args) == want;
    }
    let identity = owner_args
        .iter()
        .enumerate()
        .all(|(i, t)| *t == cx.ty.param(i as u32));
    if identity && cx.fn_info(def).ret_source == RetSource::Body {
        let f = cx.fn_info_mut(def);
        f.ret = want;
        f.ret_source = RetSource::Known;
        return true;
    }
    cx.ret_checks.push(RetCheck {
        def,
        args: owner_args.to_vec(),
        want: RetWant::Ty(want),
        span,
        message,
    });
    true
}
