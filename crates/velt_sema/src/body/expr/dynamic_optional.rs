//! An `any` / `unknown` converted to a union or literal type that also allows `null` or
//! `undefined` (`"a" | "b" | null`, `Color | null | undefined`).

use crate::body::{FnCx, LocalKind};
use crate::hir::{self, ExprKind as H, Nothing, TyId, UseMode};

impl FnCx<'_, '_> {
    /// A dynamic value as `exp`, a union or literal type `payload` that also allows `null` /
    /// `undefined`: `null` and `undefined` give the nothing, anything else is converted to
    /// `payload`.
    pub(super) fn dynamic_to_optional(
        &mut self,
        h: hir::Expr,
        exp: TyId,
        payload: TyId,
    ) -> hir::Expr {
        let span = h.span;
        let dty = h.ty;
        let init = self.share_if_place(h);
        let tmp = self.new_local("<any>", dty, false, span, LocalKind::Temp);
        let read = self.mk(H::Local(tmp, UseMode::Borrow), dty, span);
        let three = self.is_three_state(exp);
        // On `T | null | undefined`, `undefined` and `null` are its two nothings.
        let word = if three { "undefined" } else { "null" };
        let nil = self.dynamic_null_compare(read, word, three, true, span);
        let none = self.mk(H::Lit(hir::Lit::Null), exp, span);
        let read = self.mk(H::Local(tmp, UseMode::Borrow), dty, span);
        let value = match self.present_payload(exp).filter(|_| three) {
            Some(core) => {
                let v = self.unbox_dynamic(read, core);
                let v = self.present_value(v, exp, span);
                let read = self.mk(H::Local(tmp, UseMode::Borrow), dty, span);
                let is_null = self.dynamic_null_compare(read, "null", true, true, span);
                let null = self.nothing_value(Nothing::Null, exp, span);
                let kind = H::If {
                    cond: Box::new(is_null),
                    then: Box::new(null),
                    els: Box::new(v),
                };
                self.mk(kind, exp, span)
            }
            None => self.unbox_dynamic(read, payload),
        };
        let some = self.coerce(value, exp);
        let kind = H::If {
            cond: Box::new(nil),
            then: Box::new(none),
            els: Box::new(some),
        };
        let result = self.mk(kind, exp, span);
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: hir::StmtKind::Let {
                    local: tmp,
                    init: Some(init),
                },
                span,
            }],
            value: Some(Box::new(result)),
            span,
        };
        self.mk(H::Block(block), exp, span)
    }
}
