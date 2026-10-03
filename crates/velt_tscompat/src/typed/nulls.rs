//! Rules on `null` and JavaScript's `undefined`: Velt has only `null`, so a value that is
//! `undefined` in JavaScript (an optional field or parameter left out, `Map.get` of a missing
//! key, `find` without a match, `pop` of an empty array, `?.` on `null`) is `null` in Velt.
//!
//! - `strict-null-eq`: `x === null` on such a value is `false` in JavaScript (fix: `==`);
//! - `undefined-into-null`: such a value where a `T | null` is declared, which `tsc` rejects
//!   (fix: `?? null`): annotated variables and return values here, arguments and fields in
//!   [`slots`](super::slots).

use velt_common::Span;
use velt_sema::ide::{DefKind, TypeView};
use velt_syntax::ast::{self, BinaryOp, ExprKind as E, Lit, StmtKind as S};

use super::Typed;
use crate::{Fix, Severity};

/// Why `e` may be `undefined` in JavaScript where Velt has `null`; `None` when it can't be.
pub(super) fn undefined_source(e: &ast::Expr, t: &Typed) -> Option<&'static str> {
    if optional_chain(e) {
        return Some("`?.` gives `undefined` in JavaScript when the value before it is null");
    }
    match &e.kind {
        E::Paren(inner) => undefined_source(inner, t),
        E::Member { prop, .. } => {
            let d = t.def(prop.span)?;
            (d.kind == DefKind::Field && t.decls.is_optional(d.span)).then_some(
                "an optional field that an object leaves out is `undefined` in JavaScript",
            )
        }
        E::Ident(id) => ident_source(id, t),
        E::Call { callee, .. } => {
            let E::Member { object, prop, .. } = &callee.kind else {
                return None;
            };
            missing_result(&t.view_of(object), &prop.name)
        }
        _ => None,
    }
}

/// Why the variable or parameter `id` may be `undefined` in JavaScript.
pub(super) fn ident_source(id: &ast::Ident, t: &Typed) -> Option<&'static str> {
    let d = t.def(id.span)?;
    match d.kind {
        DefKind::Parameter if t.decls.is_optional(d.span) => {
            Some("an optional parameter that a call leaves out is `undefined` in JavaScript")
        }
        DefKind::Local if t.undefined_locals.contains(&d.span) => Some(
            "the variable holds a value that is `undefined` in JavaScript where Velt has `null`",
        ),
        _ => None,
    }
}

/// The methods that return `undefined` in JavaScript for "nothing" (Velt: `null`).
fn missing_result(receiver: &TypeView, method: &str) -> Option<&'static str> {
    match (receiver, method) {
        (TypeView::Map(..), "get") => {
            Some("`Map.get` of a missing key is `undefined` in JavaScript")
        }
        (TypeView::Array(_), "find" | "findLast") => {
            Some("`find` without a match is `undefined` in JavaScript")
        }
        (TypeView::Array(_), "pop" | "shift") => {
            Some("`pop` and `shift` of an empty array are `undefined` in JavaScript")
        }
        (TypeView::Array(_) | TypeView::Str, "at") => {
            Some("`at` past the end is `undefined` in JavaScript")
        }
        _ => None,
    }
}

/// `a?.b`, `a?.b.c`, `f?.()`: a chain with an optional link, `undefined` when it short-circuits.
fn optional_chain(e: &ast::Expr) -> bool {
    match &e.kind {
        E::Member {
            object, optional, ..
        }
        | E::Index {
            object, optional, ..
        } => *optional || optional_chain(object),
        E::Call {
            callee, optional, ..
        } => *optional || optional_chain(callee),
        E::NonNull(inner) => optional_chain(inner),
        _ => false,
    }
}

pub(super) fn is_null(e: &ast::Expr) -> bool {
    matches!(e.kind, E::Lit(Lit::Null))
}

/// `x === null` / `x !== null` where `x` may be `undefined` in JavaScript.
pub(super) fn strict_eq(
    e: &ast::Expr,
    op: BinaryOp,
    lhs: &ast::Expr,
    rhs: &ast::Expr,
    t: &mut Typed,
) {
    if !matches!(op, BinaryOp::Eq | BinaryOp::NotEq) {
        return;
    }
    let value = match (is_null(lhs), is_null(rhs)) {
        (true, false) => rhs,
        (false, true) => lhs,
        _ => return,
    };
    let between = Span::new(e.span.file, lhs.span.hi, rhs.span.lo);
    let text = t.cx.text(between);
    let strict = if op == BinaryOp::Eq { "===" } else { "!==" };
    let Some(at) = text.find(strict) else { return };
    let Some(why) = undefined_source(value, t) else {
        return;
    };
    let span = Span::new(
        e.span.file,
        between.lo + at as u32,
        between.lo + at as u32 + 3,
    );
    let loose = &strict[..2];
    let message = format!("`{strict} null` misses `undefined`, which this value is in JavaScript");
    let fix = Fix {
        span,
        replacement: loose.into(),
        title: format!("compare with `{loose}`, which matches `undefined` too"),
    };
    let notes = [
        why,
        "Velt has only `null`; in JavaScript `undefined === null` is `false`, while \
         `undefined == null` is `true`",
    ];
    let notes = [
        notes[0],
        notes[1],
        "write `== null` / `!= null`: the same test in Velt",
    ];
    t.cx.report(
        "strict-null-eq",
        Severity::Error,
        span,
        message,
        &notes,
        Some(fix),
    );
}

/// `undefined-into-null` at `e`, which `why` says may be `undefined`.
pub(super) fn into_null(e: &ast::Expr, why: &str, t: &mut Typed) {
    let fix = Fix {
        span: Span::new(e.span.file, e.span.hi, e.span.hi),
        replacement: " ?? null".into(),
        title: "turn `undefined` into `null`: `?? null`".into(),
    };
    t.cx.report(
        "undefined-into-null",
        Severity::Error,
        e.span,
        "TypeScript types this value `T | undefined`, which a `T | null` doesn't accept".into(),
        &[
            why,
            "append `?? null`: it turns `undefined` into `null` in JavaScript and changes \
             nothing in Velt",
        ],
        Some(fix),
    );
}

/// [`into_null`] if `e` may be `undefined`.
pub(super) fn check_into_null(e: &ast::Expr, t: &mut Typed) {
    if let Some(why) = undefined_source(e, t) {
        into_null(e, why, t);
    }
}

/// `const v = m.get(k)` makes `v` such a value; `const v: T | null = m.get(k)` is rejected.
pub(super) fn var_decl(v: &ast::VarDecl, t: &mut Typed) {
    let Some(init) = &v.init else { return };
    let Some(why) = undefined_source(init, t) else {
        return;
    };
    if v.ty.is_some() {
        into_null(init, why, t);
    } else if let ast::PatternKind::Ident(id) = &v.pattern.kind {
        t.undefined_locals.insert(id.span);
    }
}

/// The values a function body returns (not those of nested functions).
pub(super) fn returned(body: &ast::Block) -> Vec<&ast::Expr> {
    let mut found = vec![];
    collect_returns(&body.stmts, &mut found);
    found
}

/// The values an arrow's body returns.
pub(super) fn arrow_returned(body: &ast::ArrowBody) -> Vec<&ast::Expr> {
    match body {
        ast::ArrowBody::Expr(e) => vec![e],
        ast::ArrowBody::Block(b) => returned(b),
    }
}

fn collect_returns<'a>(stmts: &'a [ast::Stmt], out: &mut Vec<&'a ast::Expr>) {
    for s in stmts {
        match &s.kind {
            S::Return(Some(e)) => out.push(e),
            S::If { then, els, .. } => {
                collect_returns(&then.stmts, out);
                if let Some(els) = els {
                    collect_returns(std::slice::from_ref(els), out);
                }
            }
            S::While { body, .. } | S::DoWhile { body, .. } | S::For { body, .. } => {
                collect_returns(&body.stmts, out)
            }
            S::ForOf { body, .. } | S::Block(body) => collect_returns(&body.stmts, out),
            S::Labeled { body, .. } => collect_returns(std::slice::from_ref(body), out),
            S::Switch { cases, .. } => cases.iter().for_each(|c| collect_returns(&c.body, out)),
            S::Try {
                body,
                catch,
                finally,
            } => {
                collect_returns(&body.stmts, out);
                if let Some((_, b)) = catch {
                    collect_returns(&b.stmts, out);
                }
                if let Some(b) = finally {
                    collect_returns(&b.stmts, out);
                }
            }
            _ => {}
        }
    }
}
