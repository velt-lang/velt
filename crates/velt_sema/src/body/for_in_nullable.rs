//! `for (const k in o) body` where `o` may be `null` or `undefined` (a `Record<string, string> |
//! undefined`, #2655). JavaScript lists no keys of a nothing and TypeScript accepts the loop,
//! narrowing `o` in its body. So it is written as
//!
//! ```text
//! if (o != null) { for (const k in o) body }
//! ```
//!
//! with `o` held once in a hidden constant first when it is not a path that reads the same value
//! again (`for (const k in f())`).

use velt_syntax::ast::{self, ExprKind as E};

use super::consume::Synth;
use super::{FnCx, Want};
use crate::hir::{self, StmtKind as S};

impl FnCx<'_, '_> {
    /// The loop above for `for (kind k in iter) body` (`iter` the parser's `Object.keys(o)`) of
    /// an `o` that may be a nothing; `false` (nothing written) for any other loop.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn for_in_nullable(
        &mut self,
        kind: ast::VarKind,
        pattern: &ast::Pattern,
        iter: &ast::Expr,
        body: &ast::Block,
        label: Option<&ast::Ident>,
        span: velt_common::Span,
        out: &mut Vec<hir::Stmt>,
    ) -> bool {
        let E::Call { callee, args, .. } = &iter.kind else {
            return false;
        };
        let (E::Member { prop, .. }, [obj]) = (&callee.kind, args.as_slice()) else {
            return false;
        };
        if callee.span.lo != callee.span.hi || prop.name != "keys" {
            return false;
        }
        let Some(t) = self.trial_type(obj) else {
            return false;
        };
        if self.cx.ty.nothing_of(t).is_none() || self.cx.ty.core(t).is_none() {
            return false;
        }
        let n = self.f.locals.len();
        let syn = Synth::new(span, n);
        self.push_scope_until(span.hi);
        let mut stmts = vec![];
        let operand = match super::expr::object_keys::reevaluable(obj) {
            true => obj.clone(),
            false => {
                let held = format!("<forinn#{n}>");
                let h = self.expr(obj, None, Want::Borrow);
                let init = self.share_if_place(h);
                self.hidden_local(syn.ident(&held), init, false, &mut stmts);
                syn.name(&held)
            }
        };
        // The parser's loop, over the value once it is known to be there.
        let mut iter = iter.clone();
        if let E::Call { args, .. } = &mut iter.kind {
            args[0] = operand.clone();
        }
        let mut lp = ast::Stmt {
            kind: ast::StmtKind::ForOf {
                kind,
                pattern: pattern.clone(),
                iter,
                body: body.clone(),
                is_await: false,
            },
            span,
        };
        if let Some(label) = label {
            lp = ast::Stmt {
                kind: ast::StmtKind::Labeled {
                    label: label.clone(),
                    body: Box::new(lp),
                },
                span,
            };
        }
        let null = syn.expr(E::Lit(ast::Lit::Null));
        let test = syn.stmt(ast::StmtKind::If {
            cond: syn.binary(ast::BinaryOp::NotEq, operand, null),
            then: syn.block(vec![lp]),
            els: None,
        });
        self.stmt(&test, &mut stmts);
        self.pop_scope();
        let block = hir::Block {
            stmts,
            value: None,
            span,
        };
        Self::push(out, S::Block(block), span);
        true
    }
}
