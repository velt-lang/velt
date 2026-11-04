//! `perf/foreach-capture`: `xs.forEach((x) => { … })` whose callback assigns a variable declared
//! outside it. The closure must share that variable with the enclosing function, so the variable
//! lives in a heap cell that every read and write goes through, in the loop and after it.
//! `for (const x of xs) { … }` runs the same body in place (measured: a third of the
//! instructions for a sum over a number array). The rewrite is TypeScript too, and always a
//! Review fix (`--fix=review`): `for…of` also visits elements the body adds ([`rewrite`]).
//!
//! Only locals and parameters count as outer variables: a module-level `let` lives in module
//! state whether a closure assigns it or not, so a callback assigning one is not reported.

use velt_common::{Applicability, Fix, Span};
use velt_sema::ide::{DefKind, TypeView};
use velt_syntax::ast::{self, ArrowBody, ExprKind, StmtKind};
use velt_syntax::visit::{walk_module, Visit};

use super::Cx;
use crate::{Finding, ProfiledFix};

pub(crate) const CODE: &str = "perf/foreach-capture";

pub(super) fn find(cx: &Cx) -> Vec<Finding> {
    struct Stmts<'c, 'a> {
        cx: &'c Cx<'a>,
        out: Vec<Finding>,
    }
    impl<'a> Visit<'a> for Stmts<'_, '_> {
        fn stmt(&mut self, s: &'a ast::Stmt) {
            if let StmtKind::Expr(e) = &s.kind {
                self.out.extend(check(self.cx, s.span, e));
            }
        }
    }
    let mut v = Stmts { cx, out: vec![] };
    walk_module(&cx.module.ast, &mut v);
    v.out
}

/// The finding for the statement `stmt` whose expression is `e`, if it is such a `forEach`.
fn check(cx: &Cx, stmt: Span, e: &ast::Expr) -> Option<Finding> {
    let ExprKind::Call { callee, args, .. } = &e.kind else {
        return None;
    };
    let ExprKind::Member {
        object,
        prop,
        optional: false,
    } = &callee.kind
    else {
        return None;
    };
    let [arrow] = args.as_slice() else {
        return None;
    };
    let ExprKind::Arrow {
        is_async: false, ..
    } = &arrow.kind
    else {
        return None;
    };
    if prop.name != "forEach" || !matches!(cx.view(object.span), Some(TypeView::Array(_))) {
        return None;
    }
    let assigned = outer_assigned(cx, arrow)?;
    let mut notes = vec![
        format!(
            "a closure that assigns `{assigned}`, a variable of the enclosing function, keeps it \
             in a heap cell: every read and write of it goes through a pointer, in the loop and \
             after it"
        ),
        "`for…of` runs the same body in place, without a closure".to_string(),
    ];
    let fix = rewrite(cx, stmt, object, arrow);
    if fix.is_none() {
        notes.push(
            "rewrite it as a `for…of` loop (a `return` in the callback becomes `continue`)".into(),
        );
    }
    Some(Finding {
        code: CODE,
        span: callee.span,
        message: format!(
            "this `forEach` callback assigns `{assigned}`, which is declared outside it"
        ),
        notes,
        fixes: fix.into_iter().collect(),
    })
}

/// The first variable of the enclosing function that the callback `arrow` assigns (in nested
/// closures too: they need the cell as well).
fn outer_assigned(cx: &Cx, arrow: &ast::Expr) -> Option<String> {
    let span = arrow.span;
    let outer = |target: &ast::Expr| -> bool {
        let ExprKind::Ident(id) = &target.kind else {
            return false;
        };
        cx.analysis.def_of(id.span).is_some_and(|d| {
            matches!(d.kind, DefKind::Local | DefKind::Parameter)
                && d.span.file == span.file
                && !(span.lo <= d.span.lo && d.span.hi <= span.hi)
        })
    };
    struct Assigned<'f, 'c> {
        outer: &'f dyn Fn(&ast::Expr) -> bool,
        cx: &'f Cx<'c>,
        found: Option<String>,
    }
    impl<'a> Visit<'a> for Assigned<'_, '_> {
        fn expr(&mut self, e: &'a ast::Expr) {
            let (ExprKind::Assign { target, .. } | ExprKind::Update { target, .. }) = &e.kind
            else {
                return;
            };
            if self.found.is_none() && (self.outer)(target) {
                self.found = Some(self.cx.text(target.span).to_string());
            }
        }
    }
    let mut v = Assigned {
        outer: &outer,
        cx,
        found: None,
    };
    velt_syntax::visit::walk_expr(arrow, &mut v);
    v.found
}

/// Whether the statements return (outside nested functions, which `return` for themselves).
fn returns(stmts: &[ast::Stmt]) -> bool {
    stmts.iter().any(|s| match &s.kind {
        StmtKind::Return(_) => true,
        StmtKind::If { then, els, .. } => {
            returns(&then.stmts)
                || els
                    .as_deref()
                    .is_some_and(|e| returns(std::slice::from_ref(e)))
        }
        StmtKind::While { body, .. }
        | StmtKind::DoWhile { body, .. }
        | StmtKind::For { body, .. }
        | StmtKind::ForOf { body, .. }
        | StmtKind::Block(body) => returns(&body.stmts),
        StmtKind::Labeled { body, .. } => returns(std::slice::from_ref(body)),
        StmtKind::Switch { cases, .. } => cases.iter().any(|c| returns(&c.body)),
        StmtKind::Try {
            body,
            catch,
            finally,
        } => {
            returns(&body.stmts)
                || catch.as_ref().is_some_and(|(_, b)| returns(&b.stmts))
                || finally.as_ref().is_some_and(|b| returns(&b.stmts))
        }
        _ => false,
    })
}

/// `for (const x of xs) { … }` for the statement `stmt`, when the callback has one plain
/// parameter and no `return`. Always Review: `forEach` visits the elements there when it starts
/// and `for…of` also those added while it walks, and almost any body can run code that adds
/// one (a call, a getter, `toString` or `valueOf` behind `+` or a template, an iterator, a
/// destructuring), so no reading of the body proves the two the same.
fn rewrite(cx: &Cx, stmt: Span, object: &ast::Expr, arrow: &ast::Expr) -> Option<ProfiledFix> {
    let ExprKind::Arrow { params, body, .. } = &arrow.kind else {
        return None;
    };
    let [p] = params.as_slice() else {
        return None;
    };
    if p.rest || p.default.is_some() || p.optional {
        return None;
    }
    let binding = match &p.pattern {
        Some(pattern) => cx.text(pattern.span),
        None => p.name.name.as_str(),
    };
    let body_text = match body {
        ArrowBody::Block(b) if returns(&b.stmts) => return None,
        ArrowBody::Block(b) => cx.text(b.span).to_string(),
        ArrowBody::Expr(e) => format!("{{ {}; }}", cx.text(e.span)),
    };
    let array = cx.text(object.span);
    let text = format!("for (const {binding} of {array}) {body_text}");
    Some(ProfiledFix {
        fix: Fix::replace(
            format!("rewrite as `for (const {binding} of {array})`"),
            stmt,
            text,
            Applicability::Review,
        ),
        portable: true,
    })
}
