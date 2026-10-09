//! Whether code refers to any of a set of names: a syntactic over-approximation (shadowing is
//! ignored, and a function expression counts as referring to every name), for caches that are
//! only valid when the code does not depend on those names.

use velt_syntax::ast;

use crate::ast_walk::children;

/// Does `e` (including the bodies of arrows in it) mention any of `names`?
pub(crate) fn mentions(e: &ast::Expr, names: &[String]) -> bool {
    if names.is_empty() {
        return false;
    }
    let mut found = false;
    expr(e, names, &mut found);
    found
}

fn expr(e: &ast::Expr, names: &[String], found: &mut bool) {
    if *found {
        return;
    }
    match &e.kind {
        ast::ExprKind::Ident(id) if names.contains(&id.name) => *found = true,
        ast::ExprKind::Function(_) => *found = true,
        ast::ExprKind::Arrow {
            body: ast::ArrowBody::Block(b),
            ..
        } => block(b, names, found),
        _ => children(e, &mut |c| expr(c, names, found)),
    }
}

fn block(b: &ast::Block, names: &[String], found: &mut bool) {
    for s in &b.stmts {
        stmt(s, names, found);
    }
}

fn stmt(s: &ast::Stmt, names: &[String], found: &mut bool) {
    use ast::StmtKind as S;
    let mut e = |x: &ast::Expr| expr(x, names, found);
    match &s.kind {
        S::Var(v) => {
            if let Some(i) = &v.init {
                e(i);
            }
            pattern(&v.pattern, names, found);
        }
        S::Expr(x) | S::Throw(x) | S::Return(Some(x)) => e(x),
        S::If { cond, then, els } => {
            e(cond);
            block(then, names, found);
            if let Some(s) = els {
                stmt(s, names, found);
            }
        }
        S::While { cond, body } | S::DoWhile { body, cond } => {
            e(cond);
            block(body, names, found);
        }
        S::For {
            init,
            cond,
            update,
            body,
        } => {
            if let Some(i) = init {
                stmt(i, names, found);
            }
            for x in cond.iter().chain(update) {
                expr(x, names, found);
            }
            block(body, names, found);
        }
        S::ForOf {
            pattern: p,
            iter,
            body,
            ..
        } => {
            e(iter);
            pattern(p, names, found);
            block(body, names, found);
        }
        S::Block(b) => block(b, names, found),
        S::Switch {
            discriminant,
            cases,
        } => {
            e(discriminant);
            for c in cases {
                if let Some(t) = &c.test {
                    expr(t, names, found);
                }
                for s in &c.body {
                    stmt(s, names, found);
                }
            }
        }
        S::Labeled { body, .. } => stmt(body, names, found),
        S::Try {
            body,
            catch,
            finally,
        } => {
            block(body, names, found);
            if let Some((_, b)) = catch {
                block(b, names, found);
            }
            if let Some(b) = finally {
                block(b, names, found);
            }
        }
        S::Return(None) | S::Break(_) | S::Continue(_) | S::Item(_) | S::Empty => {}
    }
}

/// Default values in a destructuring pattern.
fn pattern(p: &ast::Pattern, names: &[String], found: &mut bool) {
    use ast::PatternKind as P;
    match &p.kind {
        P::Ident(_) | P::Wildcard => {}
        P::Object { fields, .. } => fields.iter().for_each(|(_, p)| pattern(p, names, found)),
        P::Array { elems, .. } => elems.iter().for_each(|p| pattern(p, names, found)),
        P::Default { pattern: q, value } => {
            pattern(q, names, found);
            expr(value, names, found);
        }
    }
}
