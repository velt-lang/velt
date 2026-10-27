//! `for...of` over an `any` (#924): the value must hold an array, whose elements the loop visits
//! as `any`s, reading the length before each step as JavaScript's array iterator does:
//!
//! ```text
//! { let <array> = it;  // checked: an array, else a TypeError
//!   for (let <index> = 0; <index> < <array>.length; <index>++) {
//!     kind pattern = <array>[<index>]; body } }
//! ```
//!
//! Another value (a string, a `Map`, an iterable object) stops the program with a `TypeError`
//! saying it is not supported through `any` yet, never a loop over the wrong values.

use velt_syntax::ast::{self, ExprKind as E};

use super::consume::Synth;
use super::for_iter::ForOfParts;
use super::FnCx;
use crate::hir::{self, ExprKind as H, Intrinsic, StmtKind as S, UseMode};

impl FnCx<'_, '_> {
    /// `for (kind pattern of it) body` where `it` is an `any`.
    pub(super) fn for_of_any(
        &mut self,
        it: hir::Expr,
        p: ForOfParts<'_>,
        out: &mut Vec<hir::Stmt>,
    ) {
        let span = p.span;
        let syn = Synth::new(span, self.f.locals.len());
        let (bool_, never) = (self.cx.ty.bool_, self.cx.ty.never);
        self.push_scope_until(span.hi);
        let mut stmts = vec![];
        let init = self.share_if_place(it);
        let dty = init.ty;
        let array = self.hidden_local(syn.ident(&syn.array), init, false, &mut stmts);
        // Not an array: a `TypeError`.
        let read = self.mk(H::Local(array, UseMode::Borrow), dty, p.iter_span);
        let is_array = self.intrinsic(Intrinsic::AnyIsArray, vec![read], bool_, p.iter_span);
        let msg = self.str_lit(
            "TypeError: `for...of` through `any` iterates arrays; this value is not one, which is not supported yet (#924)",
            p.iter_span,
        );
        let stop = self.intrinsic(Intrinsic::Panic, vec![msg], never, p.iter_span);
        stmts.push(hir::Stmt {
            kind: S::If {
                cond: is_array,
                then: hir::Block {
                    stmts: vec![],
                    value: None,
                    span: p.iter_span,
                },
                els: Some(hir::Block {
                    stmts: vec![hir::Stmt {
                        kind: S::Expr(stop),
                        span: p.iter_span,
                    }],
                    value: None,
                    span: p.iter_span,
                }),
            },
            span: p.iter_span,
        });
        let index = syn.index_name();
        let init = syn.stmt(ast::StmtKind::Var(ast::VarDecl {
            kind: ast::VarKind::Let,
            pattern: syn.ident_pat(&index),
            ty: None,
            init: Some(syn.int(0)),
            span: syn.at,
        }));
        let length = syn.member(syn.name(&syn.array), "length");
        let cond = syn.binary(ast::BinaryOp::Lt, syn.name(&index), length);
        let update = syn.expr(E::Update {
            op: ast::UpdateOp::Inc,
            prefix: false,
            target: Box::new(syn.name(&index)),
        });
        let element = syn.expr(E::Index {
            object: Box::new(syn.name(&syn.array)),
            index: Box::new(syn.name(&index)),
            optional: false,
        });
        let decl = syn.stmt(ast::StmtKind::Var(ast::VarDecl {
            kind: p.kind,
            pattern: p.pattern.clone(),
            ty: None,
            init: Some(element),
            span: p.pattern.span,
        }));
        let body = ast::Stmt {
            kind: ast::StmtKind::Block(p.body.clone()),
            span: p.body.span,
        };
        let lp = ast::Stmt {
            kind: ast::StmtKind::For {
                init: Some(Box::new(init)),
                cond: Some(cond),
                update: Some(update),
                body: syn.block(vec![decl, body]),
            },
            span,
        };
        self.loop_stmt(&lp, p.label, &mut stmts);
        self.pop_scope();
        let block = hir::Block {
            stmts,
            value: None,
            span,
        };
        Self::push(out, S::Block(block), span);
    }
}
