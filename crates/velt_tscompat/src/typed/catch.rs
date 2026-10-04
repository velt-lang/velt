//! `catch-unknown`: Velt types a caught value with what the `try` block throws, TypeScript (in
//! `strict` mode) with `unknown`, so `e.message` in `catch (e)` compiles in Velt only. A member
//! is fine where `e instanceof C` narrows it: in the `if` branch, after `&&`, in the `?` branch.

use velt_common::Span;
use velt_sema::ide::DefKind;
use velt_syntax::ast::{self, BinaryOp, ExprKind as E, StmtKind as S};

use super::Typed;

/// The catch bindings and where `instanceof` narrows a value.
#[derive(Default)]
pub(super) struct State {
    /// Declaring identifiers of `catch (e)` bindings.
    bindings: Vec<Span>,
    /// Regions where the variable declared at the second span is narrowed by `instanceof`.
    narrowed: Vec<(Span, Span)>,
}

/// `try … catch (e)` declares a binding; `if (e instanceof C)` narrows it in its branch.
pub(super) fn stmt(s: &ast::Stmt, t: &mut Typed) {
    match &s.kind {
        S::Try {
            catch: Some((Some(pattern), _)),
            ..
        } => {
            if let ast::PatternKind::Ident(id) = &pattern.kind {
                t.catch.bindings.push(id.span);
            }
        }
        S::If { cond, then, .. } => narrow(cond, then.span, t),
        _ => {}
    }
}

/// `e instanceof C && e.x`.
pub(super) fn binary(op: BinaryOp, lhs: &ast::Expr, rhs: &ast::Expr, t: &mut Typed) {
    if op == BinaryOp::And {
        narrow(lhs, rhs.span, t);
    }
}

/// `e instanceof C ? e.x : …`.
pub(super) fn cond(test: &ast::Expr, then: &ast::Expr, t: &mut Typed) {
    narrow(test, then.span, t);
}

/// Every `x instanceof C` in the `&&` chain `test` narrows `x` in `region`.
fn narrow(test: &ast::Expr, region: Span, t: &mut Typed) {
    if t.catch.bindings.is_empty() {
        return;
    }
    match &test.kind {
        E::Paren(inner) => narrow(inner, region, t),
        E::Binary {
            op: BinaryOp::And,
            lhs,
            rhs,
        } => {
            narrow(lhs, region, t);
            narrow(rhs, region, t);
        }
        E::InstanceOf { expr, .. } => {
            if let E::Ident(id) = &expr.kind {
                if let Some(d) = t.def(id.span) {
                    t.catch.narrowed.push((region, d.span));
                }
            }
        }
        _ => {}
    }
}

/// `e.x` / `e[i]` where `object` is a caught value that isn't narrowed there.
pub(super) fn member(object: &ast::Expr, t: &mut Typed) {
    let E::Ident(id) = &object.kind else { return };
    if t.catch.bindings.is_empty() {
        return;
    }
    let Some(d) = t.def(id.span) else { return };
    if d.kind != DefKind::Local || !t.catch.bindings.contains(&d.span) {
        return;
    }
    let inside = |r: Span| r.file == id.span.file && r.lo <= id.span.lo && id.span.hi <= r.hi;
    if t.catch
        .narrowed
        .iter()
        .any(|(region, binding)| *binding == d.span && inside(*region))
    {
        return;
    }
    t.cx.error(
        "catch-unknown",
        id.span,
        format!(
            "TypeScript types the caught value `{}` as `unknown`, so it has no members",
            id.name
        ),
        &[
            "Velt knows what the `try` block throws; TypeScript (`strict`) types a caught \
             value `unknown` and rejects `e.message` until it is narrowed",
            "narrow it first: `if (e instanceof Error) { … e.message … }`",
        ],
    );
}
