//! Which variables a statement may assign (`x = …`, `x += …`, `x++`), closures included — a
//! syntactic over-approximation used to drop flow narrowing around loops.

use std::collections::HashSet;

use velt_syntax::ast;

/// Names assigned anywhere in `s`.
pub(crate) fn assigned_in_stmt<'a>(s: &'a ast::Stmt, out: &mut HashSet<&'a str>) {
    use ast::StmtKind as S;
    match &s.kind {
        S::Var(v) => v.init.iter().for_each(|e| assigned_in_expr(e, out)),
        S::Expr(e) | S::Throw(e) | S::Return(Some(e)) => assigned_in_expr(e, out),
        S::If { cond, then, els } => {
            assigned_in_expr(cond, out);
            assigned_in_block(then, out);
            els.iter().for_each(|e| assigned_in_stmt(e, out));
        }
        S::While { cond, body } | S::DoWhile { body, cond } => {
            assigned_in_expr(cond, out);
            assigned_in_block(body, out);
        }
        S::For {
            init,
            cond,
            update,
            body,
        } => {
            init.iter().for_each(|i| assigned_in_stmt(i, out));
            cond.iter()
                .chain(update)
                .for_each(|e| assigned_in_expr(e, out));
            assigned_in_block(body, out);
        }
        S::ForOf { iter, body, .. } => {
            assigned_in_expr(iter, out);
            assigned_in_block(body, out);
        }
        S::Block(b) => assigned_in_block(b, out),
        S::Switch {
            discriminant,
            cases,
        } => {
            assigned_in_expr(discriminant, out);
            for c in cases {
                c.test.iter().for_each(|e| assigned_in_expr(e, out));
                c.body.iter().for_each(|s| assigned_in_stmt(s, out));
            }
        }
        S::Labeled { body, .. } => assigned_in_stmt(body, out),
        S::Try {
            body,
            catch,
            finally,
        } => {
            assigned_in_block(body, out);
            catch.iter().for_each(|(_, b)| assigned_in_block(b, out));
            finally.iter().for_each(|b| assigned_in_block(b, out));
        }
        S::Return(None) | S::Break(_) | S::Continue(_) | S::Item(_) | S::Empty => {}
    }
}

fn assigned_in_block<'a>(b: &'a ast::Block, out: &mut HashSet<&'a str>) {
    b.stmts.iter().for_each(|s| assigned_in_stmt(s, out));
}

fn assigned_in_expr<'a>(e: &'a ast::Expr, out: &mut HashSet<&'a str>) {
    match &e.kind {
        ast::ExprKind::Assign { target, .. } | ast::ExprKind::Update { target, .. } => {
            if let ast::ExprKind::Ident(id) = &target.kind {
                out.insert(id.name.as_str());
            }
        }
        ast::ExprKind::Arrow {
            body: ast::ArrowBody::Block(b),
            ..
        } => assigned_in_block(b, out),
        _ => {}
    }
    crate::ast_walk::children(e, &mut |c| assigned_in_expr(c, out));
}
