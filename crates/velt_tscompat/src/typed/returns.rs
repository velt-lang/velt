//! The values a function or arrow returns, for the rules that check them against a declared
//! return type.

use velt_syntax::ast::{self, StmtKind as S};

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
