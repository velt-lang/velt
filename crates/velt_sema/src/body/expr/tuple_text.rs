//! `t.join(sep)` and `t.toString()` on a tuple (issue #1462), as `Array.prototype.join` writes
//! an array (a tuple is one in JavaScript): each element as `String(x)` writes it, `null` and
//! `undefined` as empty text, the receiver evaluated before the separator.

use velt_common::Span;
use velt_syntax::ast;

use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::hir::{self, TyKind};

/// `t[i]`.
fn element(t: &ast::Expr, i: usize, span: Span) -> ast::Expr {
    let index = ast::Lit::Int {
        value: i as u128,
        suffix: None,
    };
    let kind = ast::ExprKind::Index {
        object: Box::new(t.clone()),
        index: Box::new(synth(ast::ExprKind::Lit(index), span)),
        optional: false,
    };
    synth(kind, span)
}

fn binary(op: ast::BinaryOp, lhs: ast::Expr, rhs: ast::Expr, span: Span) -> ast::Expr {
    let kind = ast::ExprKind::Binary {
        op,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    };
    synth(kind, span)
}

fn string_of(e: ast::Expr, span: Span) -> ast::Expr {
    let callee = ast::Ident {
        name: "String".into(),
        span,
    };
    let kind = ast::ExprKind::Call {
        callee: Box::new(synth(ast::ExprKind::Ident(callee), span)),
        type_args: vec![],
        args: vec![e],
        optional: false,
    };
    synth(kind, span)
}

fn str_lit(s: &str, span: Span) -> ast::Expr {
    synth(ast::ExprKind::Lit(ast::Lit::Str(s.into())), span)
}

impl FnCx<'_, '_> {
    /// `recv.join(sep)` / `recv.toString()` when `recv` is a tuple; `Err` gives `recv` back for
    /// other receivers and methods.
    pub(super) fn tuple_text_call(
        &mut self,
        recv: hir::Expr,
        prop: &ast::Ident,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let TyKind::Tuple(elems) = self.cx.ty.kind(recv.ty).clone() else {
            return Err(recv);
        };
        let max = match prop.name.as_str() {
            "join" => 1,
            "toString" => 0,
            _ => return Err(recv),
        };
        if args.len() > max {
            return Err(recv);
        }
        let nullable: Vec<bool> = elems
            .iter()
            .map(|&e| self.cx.ty.opt_payload(e).is_some())
            .collect();
        let recv = self.share_if_place(recv);
        Ok(self.with_temp("<tuple>", recv, span, span, |lw, t| {
            let str_ = lw.cx.ty.str_;
            let sep = match args.first() {
                Some(a) => {
                    let h = lw.expr(a, Some(str_), Want::Move);
                    lw.coerce(h, str_)
                }
                None => lw.str_lit(",", span),
            };
            lw.with_temp("<sep>", sep, span, span, |lw, s| {
                let mut text: Option<ast::Expr> = None;
                for (i, &nullable) in nullable.iter().enumerate() {
                    let e = element(t, i, span);
                    let part = if nullable {
                        let null = synth(ast::ExprKind::Lit(ast::Lit::Null), span);
                        let test = binary(ast::BinaryOp::Eq, e.clone(), null, span);
                        let kind = ast::ExprKind::Cond {
                            cond: Box::new(test),
                            then: Box::new(str_lit("", span)),
                            els: Box::new(string_of(e, span)),
                        };
                        synth(kind, span)
                    } else {
                        string_of(e, span)
                    };
                    text = Some(match text {
                        None => binary(ast::BinaryOp::Add, str_lit("", span), part, span),
                        Some(acc) => {
                            let acc = binary(ast::BinaryOp::Add, acc, s.clone(), span);
                            binary(ast::BinaryOp::Add, acc, part, span)
                        }
                    });
                }
                let text = text.unwrap_or_else(|| str_lit("", span));
                lw.expr(&text, Some(str_), Want::Move)
            })
        }))
    }
}
