//! `+` with an `any` operand and a string (#1242): the `any` is written as `String(x)` writes
//! it, in `s + a`, `a + s` and `s += a`.

use velt_common::Span;

use crate::any::Dynamic;
use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, Intrinsic, TyId};

impl FnCx<'_, '_> {
    /// `l + r` with an `any` operand: when the other operand is a string, the `any` is written
    /// as `String(x)` writes it (`"" + e` of an error is `Error: boom`, #1242); any other `+`
    /// is [`Self::dynamic_operator`]'s.
    pub(crate) fn dynamic_concat(&mut self, l: hir::Expr, r: hir::Expr, span: Span) -> hir::Expr {
        let s = self.cx.ty.str_;
        let is_any = |cx: &mut Self, t: TyId| cx.cx.dynamic(t) == Some(Dynamic::Any);
        let (lw, rw) = (self.cx.widened(l.ty), self.cx.widened(r.ty));
        if lw == s && is_any(self, r.ty) {
            let r = self.js_text(r, "a string concatenation");
            return self.concat(l, r, span);
        }
        if rw == s && is_any(self, l.ty) {
            let l = self.js_text(l, "a string concatenation");
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
}
