//! Conversions between `any` / `unknown` and declared enums without payloads, whose values
//! are numbers or strings in JavaScript (dynamic.rs).

use velt_common::Span;

use crate::body::{FnCx, LocalKind};
use crate::hir::{self, ExprKind as H, Intrinsic, PatKind as P, TyId, TyKind, UseMode};

impl FnCx<'_, '_> {
    /// The enum of `t` when it is a declared enum without payloads (`enum Color { Red }`), whose
    /// values are numbers or strings in JavaScript.
    pub(super) fn plain_enum(&self, t: TyId) -> Option<hir::DefId> {
        if self.cx.union_def(t).is_some() || !self.is_c_like_enum(t) {
            return None;
        }
        match self.cx.ty.kind(t) {
            TyKind::Adt(d, _) => Some(*d),
            _ => None,
        }
    }

    /// The JavaScript value of each member of the enum `def`: its number or its string.
    pub(super) fn enum_member_values(&mut self, def: hir::DefId, span: Span) -> Vec<hir::Expr> {
        let values: Vec<Result<String, i64>> = self
            .cx
            .enum_info(def)
            .map(|e| {
                e.variants
                    .iter()
                    .map(|v| v.str_value.clone().ok_or(v.discriminant))
                    .collect()
            })
            .unwrap_or_default();
        values
            .into_iter()
            .map(|v| match v {
                Ok(s) => self.str_lit(&s, span),
                Err(n) => self.mk(H::Lit(hir::Lit::Float(n as f64)), self.cx.ty.f64, span),
            })
            .collect()
    }

    /// An enum value boxed as its number or string, as JavaScript holds it.
    pub(super) fn enum_to_dynamic(
        &mut self,
        h: hir::Expr,
        def: hir::DefId,
        exp: TyId,
    ) -> hir::Expr {
        let (span, ety) = (h.span, h.ty);
        let values = self.enum_member_values(def, span);
        let arms = values
            .into_iter()
            .enumerate()
            .map(|(i, v)| hir::Arm {
                pat: self.pat(
                    P::Variant {
                        def,
                        variant: i as u32,
                        args: vec![],
                    },
                    ety,
                    span,
                ),
                guard: None,
                body: self.coerce(v, exp),
            })
            .collect();
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        self.mk(kind, exp, span)
    }

    /// A dynamic value as the enum `exp`: the member whose number or string it is (`===`), or a
    /// `TypeError`.
    pub(super) fn dynamic_to_enum(
        &mut self,
        h: hir::Expr,
        def: hir::DefId,
        exp: TyId,
    ) -> hir::Expr {
        let span = h.span;
        let dty = h.ty;
        let Some(any) = self.any_ty() else {
            return self.error_expr(span);
        };
        let type_args = match self.cx.ty.kind(exp) {
            TyKind::Adt(_, a) => a.clone(),
            _ => vec![],
        };
        let init = self.share_if_place(h);
        let tmp = self.new_local("<any>", dty, false, span, LocalKind::Temp);
        let name = self.cx.display(exp);
        let msg = self.str_lit(&format!("TypeError: the value is not a {name}"), span);
        let never = self.cx.ty.never;
        let b = self.cx.ty.bool_;
        let mut result = self.intrinsic(Intrinsic::Panic, vec![msg], never, span);
        let values = self.enum_member_values(def, span);
        for (i, v) in values.into_iter().enumerate().rev() {
            let boxed = self.coerce(v, any);
            let x = self.mk(H::Local(tmp, UseMode::Borrow), dty, span);
            let test = self.intrinsic(Intrinsic::AnyStrictEq, vec![x, boxed], b, span);
            let member = self.mk(
                H::Variant {
                    def,
                    type_args: type_args.clone(),
                    variant: i as u32,
                    args: vec![],
                },
                exp,
                span,
            );
            let kind = H::If {
                cond: Box::new(test),
                then: Box::new(member),
                els: Box::new(result),
            };
            result = self.mk(kind, exp, span);
        }
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
