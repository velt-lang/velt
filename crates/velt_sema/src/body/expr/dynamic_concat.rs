//! `+` with an `any` operand and a string (#1242): the `any` is written as `String(x)` writes
//! it, in `s + a`, `a + s` and `s += a`.

use velt_common::Span;

use crate::any::Dynamic;
use crate::body::{FnCx, LocalKind};
use crate::hir::{self, BinOp, ExprKind as H, Intrinsic, StmtKind as S, TyId, UseMode};

impl FnCx<'_, '_> {
    /// `l + r` with an `any` operand: when the other operand is a string, the `any` is written
    /// as `String(x)` writes it (`"" + e` of an error is `Error: boom`, #1242); any other `+`
    /// is [`Self::dynamic_operator`]'s.
    pub(crate) fn dynamic_concat(&mut self, l: hir::Expr, r: hir::Expr, span: Span) -> hir::Expr {
        let s = self.cx.ty.str_;
        let is_any = |cx: &mut Self, t: TyId| cx.cx.dynamic(t) == Some(Dynamic::Any);
        let (lw, rw) = (self.cx.widened(l.ty), self.cx.widened(r.ty));
        if lw == s && is_any(self, r.ty) {
            let r = self.implicit_any_text(r, "a string concatenation");
            return self.concat(l, r, span);
        }
        if rw == s && is_any(self, l.ty) {
            let l = self.implicit_any_text(l, "a string concatenation");
            return self.concat(l, r, span);
        }
        self.dynamic_operator("+", &l, &r, span)
    }

    /// `h` when it is an `any`, or the `any` value `h` converts when `h` is its checked
    /// conversion (`AnyTo`, of a shared place or of a value); else `Err(h)`.
    pub(crate) fn any_to_source(&mut self, h: hir::Expr) -> Result<hir::Expr, hir::Expr> {
        if self.cx.dynamic(h.ty) == Some(Dynamic::Any) {
            return Ok(h);
        }
        let H::Call {
            callee: hir::Callee::Intrinsic(Intrinsic::AnyTo),
            args,
        } = &h.kind
        else {
            return Err(h);
        };
        let [a] = args.as_slice() else {
            return Err(h);
        };
        if self.cx.dynamic(a.ty) != Some(Dynamic::Any) {
            return Err(h);
        }
        Ok(match &a.kind {
            H::Call {
                callee: hir::Callee::Intrinsic(Intrinsic::Share),
                args,
            } if args.len() == 1 => args[0].clone(),
            _ => a.clone(),
        })
    }

    /// The text of the `any` `h` in a template literal or a string `+` / `+=` (`what`), as
    /// JavaScript's `ToString` writes it: a symbol stops with Node's `TypeError: Cannot convert
    /// a Symbol value to a string` (only `String(s)` writes a symbol, as `Symbol(desc)`).
    pub(crate) fn implicit_any_text(&mut self, h: hir::Expr, what: &str) -> hir::Expr {
        if self.cx.dynamic(h.ty) != Some(Dynamic::Any) {
            return self.js_text(h, what);
        }
        let (span, dty) = (h.span, h.ty);
        let (str_, bool_, never) = (self.cx.ty.str_, self.cx.ty.bool_, self.cx.ty.never);
        let init = self.share_if_place(h);
        let tmp = self.new_local("<any>", dty, false, span, LocalKind::Temp);
        let read = self.mk(H::Local(tmp, UseMode::Borrow), dty, span);
        let of = self.intrinsic(Intrinsic::AnyTypeof, vec![read], str_, span);
        let want = self.str_lit("symbol", span);
        let kind = H::Binary {
            op: BinOp::Eq,
            lhs: Box::new(of),
            rhs: Box::new(want),
        };
        let is_symbol = self.mk(kind, bool_, span);
        let msg = self.str_lit("TypeError: Cannot convert a Symbol value to a string", span);
        let stop = self.intrinsic(Intrinsic::Panic, vec![msg], never, span);
        let read = self.mk(H::Local(tmp, UseMode::Borrow), dty, span);
        let text = self.js_text(read, what);
        let stmts = vec![
            hir::Stmt {
                kind: S::Let {
                    local: tmp,
                    init: Some(init),
                },
                span,
            },
            hir::Stmt {
                kind: S::If {
                    cond: is_symbol,
                    then: hir::Block {
                        stmts: vec![hir::Stmt {
                            kind: S::Expr(stop),
                            span,
                        }],
                        value: None,
                        span,
                    },
                    els: None,
                },
                span,
            },
        ];
        let ty = text.ty;
        let block = hir::Block {
            stmts,
            value: Some(Box::new(text)),
            span,
        };
        self.mk(H::Block(block), ty, span)
    }
}
