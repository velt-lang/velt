//! Divergence analysis over HIR ("does control ever fall off the end of this block?").
//! Used for "all paths must return" in non-void functions.
//!
//! A `while (true)` loop with no `break` targeting it diverges; `return`, `break`, `continue` and
//! `never`-typed expressions (`panic`, `process.exit`) diverge.

use crate::hir::{Block, Expr, ExprKind, Lit, Stmt, StmtKind};
use crate::types::Types;

pub(crate) fn block_diverges(b: &Block, ty: &Types) -> bool {
    b.stmts.iter().any(|s| stmt_diverges(s, ty))
        || b.value.as_ref().is_some_and(|v| expr_diverges(v, ty))
}

fn expr_diverges(e: &Expr, ty: &Types) -> bool {
    if e.ty == ty.never {
        return true;
    }
    match &e.kind {
        ExprKind::Block(b) => block_diverges(b, ty),
        _ => false,
    }
}

fn stmt_diverges(s: &Stmt, ty: &Types) -> bool {
    match &s.kind {
        StmtKind::Let { init, .. } => init.as_ref().is_some_and(|e| expr_diverges(e, ty)),
        StmtKind::LetPat { init, .. } => expr_diverges(init, ty),
        StmtKind::Expr(e) => expr_diverges(e, ty),
        StmtKind::Return(_) | StmtKind::Break(_) | StmtKind::Continue(_) => true,
        StmtKind::Try {
            body,
            catch,
            finally,
        } => {
            finally.as_ref().is_some_and(|f| block_diverges(f, ty))
                || (block_diverges(body, ty)
                    && catch.as_ref().is_none_or(|(_, c)| block_diverges(c, ty)))
        }
        StmtKind::If { cond, then, els } => {
            expr_diverges(cond, ty)
                || (block_diverges(then, ty) && els.as_ref().is_some_and(|b| block_diverges(b, ty)))
        }
        StmtKind::While {
            label,
            cond,
            body,
            step,
        } => {
            if expr_diverges(cond, ty) {
                return true;
            }
            let always = matches!(cond.kind, ExprKind::Lit(Lit::Bool(true)));
            always
                && !block_breaks_to(body, label.as_deref(), 0)
                && !step
                    .as_ref()
                    .is_some_and(|s| expr_breaks_to(s, label.as_deref(), 0))
        }
        StmtKind::ForOf { iter, .. } => expr_diverges(iter, ty),
        StmtKind::Block(b) => block_diverges(b, ty),
    }
}

/// Does `b` contain a `break` that exits the loop labelled `label` (`depth` = loops nested
/// between here and that loop)?
fn block_breaks_to(b: &Block, label: Option<&str>, depth: u32) -> bool {
    b.stmts.iter().any(|s| stmt_breaks_to(s, label, depth))
        || b.value
            .as_ref()
            .is_some_and(|v| expr_breaks_to(v, label, depth))
}

fn stmt_breaks_to(s: &Stmt, label: Option<&str>, depth: u32) -> bool {
    match &s.kind {
        StmtKind::Break(None) => depth == 0,
        StmtKind::Break(Some(l)) => label == Some(l.as_str()),
        StmtKind::If { cond, then, els } => {
            expr_breaks_to(cond, label, depth)
                || block_breaks_to(then, label, depth)
                || els
                    .as_ref()
                    .is_some_and(|b| block_breaks_to(b, label, depth))
        }
        StmtKind::While {
            cond, body, step, ..
        } => {
            expr_breaks_to(cond, label, depth)
                || block_breaks_to(body, label, depth + 1)
                || step
                    .as_ref()
                    .is_some_and(|e| expr_breaks_to(e, label, depth + 1))
        }
        StmtKind::ForOf { body, .. } => block_breaks_to(body, label, depth + 1),
        StmtKind::Block(b) => block_breaks_to(b, label, depth),
        StmtKind::Try {
            body,
            catch,
            finally,
        } => {
            block_breaks_to(body, label, depth)
                || catch
                    .as_ref()
                    .is_some_and(|(_, c)| block_breaks_to(c, label, depth))
                || finally
                    .as_ref()
                    .is_some_and(|f| block_breaks_to(f, label, depth))
        }
        StmtKind::Let { init: Some(e), .. } | StmtKind::Expr(e) | StmtKind::Return(Some(e)) => {
            expr_breaks_to(e, label, depth)
        }
        _ => false,
    }
}

/// Breaks can only hide inside block-valued expressions (incl. `match` arms).
fn expr_breaks_to(e: &Expr, label: Option<&str>, depth: u32) -> bool {
    match &e.kind {
        ExprKind::Block(b) => block_breaks_to(b, label, depth),
        ExprKind::Match { arms, .. } => arms.iter().any(|a| expr_breaks_to(&a.body, label, depth)),
        ExprKind::If { cond, then, els } => {
            expr_breaks_to(cond, label, depth)
                || expr_breaks_to(then, label, depth)
                || expr_breaks_to(els, label, depth)
        }
        _ => false,
    }
}
