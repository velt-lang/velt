//! `x++` and `x op= v` on a late `let` (`let x!: T;` that a closure assigns, held as a
//! `T | undefined`; `closure_assigned::late_let`): read-modify-writes through the same checked
//! read as plain reads, as `x = x op v` (the target is a variable, read once either way):
//!
//! ```text
//! x += v   →  x = x + v
//! ++x      →  x = x + 1          (its value: the new `x`)
//! x++      →  { const #postfix = x; x = #postfix + 1; #postfix }
//! ```

use velt_common::Span;
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{self, TyId};

fn expr_of(kind: ast::ExprKind, span: Span) -> ast::Expr {
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    }
}

fn binary(op: ast::BinaryOp, lhs: ast::Expr, rhs: ast::Expr, span: Span) -> ast::Expr {
    expr_of(
        ast::ExprKind::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        },
        span,
    )
}

fn assign(target: &ast::Expr, value: ast::Expr, span: Span) -> ast::Expr {
    expr_of(
        ast::ExprKind::Assign {
            op: None,
            target: Box::new(target.clone()),
            value: Box::new(value),
        },
        span,
    )
}

impl FnCx<'_, '_> {
    /// Is `target` a late `let` (module docs)?
    pub(super) fn is_late_let_target(&mut self, target: &ast::Expr) -> bool {
        let mut t = target;
        while let ast::ExprKind::Paren(x) = &t.kind {
            t = x;
        }
        let ast::ExprKind::Ident(id) = &t.kind else {
            return false;
        };
        match self.lookup_local(&id.name, id.span) {
            Some(l) => self.is_late_let(l) && self.closure_assignment(l).is_some(),
            None => false,
        }
    }

    /// Is `e` an update or compound assignment (not `??=` and the like) of a late `let`?
    pub(crate) fn late_let_rmw(&mut self, e: &ast::Expr) -> bool {
        match &e.kind {
            ast::ExprKind::Update { target, .. } => self.is_late_let_target(target),
            ast::ExprKind::Assign {
                op: Some(op),
                target,
                ..
            } => {
                !matches!(
                    op,
                    ast::BinaryOp::And | ast::BinaryOp::Or | ast::BinaryOp::Nullish
                ) && self.is_late_let_target(target)
            }
            _ => false,
        }
    }

    /// `x op= value` on a late `let`: `x = x op value` (module docs).
    pub(super) fn late_let_compound(
        &mut self,
        op: ast::BinaryOp,
        target: &ast::Expr,
        value: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let rhs = binary(op, target.clone(), value.clone(), value.span);
        self.assign_value(None, target, &rhs, exp, span)
    }

    /// `x++` / `--x` on a late `let` (module docs).
    pub(super) fn late_let_update(
        &mut self,
        op: ast::UpdateOp,
        prefix: bool,
        target: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let bop = match op {
            ast::UpdateOp::Inc => ast::BinaryOp::Add,
            ast::UpdateOp::Dec => ast::BinaryOp::Sub,
        };
        let one = expr_of(
            ast::ExprKind::Lit(ast::Lit::Int {
                value: 1,
                suffix: None,
            }),
            span,
        );
        if prefix || exp == Some(self.cx.ty.unit) {
            let rhs = binary(bop, target.clone(), one, span);
            let a = assign(target, rhs, span);
            return self.expr(&a, exp, crate::body::Want::Move);
        }
        self.push_scope();
        let mut stmts = vec![];
        let old = self.expr(target, None, crate::body::Want::Borrow);
        let name = ast::Ident {
            name: "#postfix".into(),
            span,
        };
        self.hidden_local(name.clone(), old, false, &mut stmts);
        let read = || expr_of(ast::ExprKind::Ident(name.clone()), span);
        let a = assign(target, binary(bop, read(), one, span), span);
        let unit = self.cx.ty.unit;
        let a = self.expr(&a, Some(unit), crate::body::Want::Move);
        stmts.push(hir::Stmt {
            kind: hir::StmtKind::Expr(a),
            span,
        });
        let value = self.expr(&read(), None, crate::body::Want::Move);
        self.pop_scope();
        let ty = value.ty;
        let block = hir::Block {
            stmts,
            value: Some(Box::new(value)),
            span,
        };
        self.mk(hir::ExprKind::Block(block), ty, span)
    }
}
