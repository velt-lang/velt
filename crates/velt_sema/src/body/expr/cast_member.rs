//! A member read on a union value asserted to one of its members, `(u as M).f`
//! (docs/reference/types.md "Type assertions"): as in JavaScript, where `as` converts nothing.
//! The assertion `u as M` used as a value stays checked (`narrowing_casts.rs`); read for one
//! field, the value of another member is read through `any` instead of failing the assertion:
//! `undefined` for a property it lacks, an array's or a string's own `length`, and a
//! `TypeError` for `null` or `undefined`, as in Node.
//!
//! The read has the field's type `T`. A value read through `any` converts to `T`, checked
//! (a `TypeError` where an array gives `undefined` for a `string` field), except where the
//! read is only tested (a condition, the operand of `!`, `&&` and `||` in a condition): there
//! the value read is tested as it is, so `if ((c as VNode).type)` is false for an array.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::places::is_place;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, PatKind as P, TyId, UseMode};
use crate::types::collect_params;

use super::object_builtins::strip_parens;
use super::union_coerce::outer_mode;

/// Is `e` (parentheses stripped) a member read of an `as` assertion, `(x as M).f`?
pub(crate) fn is_cast_member(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        } => matches!(strip_parens(object).kind, ast::ExprKind::Cast { .. }),
        _ => false,
    }
}

impl FnCx<'_, '_> {
    /// `(x as M).prop` read where `x` (a variable or a path of fields) is a union that has the
    /// object type `M`, with a field `prop`, as a member; `None` for any other member read.
    pub(super) fn cast_member_read(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        if want == Want::BorrowMut {
            return None;
        }
        let ast::ExprKind::Cast { expr: inner, ty } = &strip_parens(object).kind else {
            return None;
        };
        let any = self.any_ty()?;
        let from = self.peek_ty(inner)?;
        // Checked again where it is used: its errors are reported there.
        let mark = self.cx.diags.len();
        let target = self.resolve(ty);
        self.cx.diags.truncate(mark);
        let union = self.cx.ty.core(from).unwrap_or(from);
        if !self.cast_member_applies(union, target, &prop.name) {
            return None;
        }
        let test = self.cast_member_test == Some(span);
        let h = self.expr(inner, None, Want::Borrow);
        // `null` / `undefined` has no properties: Node's `TypeError`.
        let h = match self.cx.ty.nothing_of(h.ty) {
            Some(n) => {
                let msg = format!(
                    "TypeError: Cannot read properties of {} (reading '{}')",
                    n.word(),
                    prop.name
                );
                self.checked_present(h, &msg, None, false, span)
            }
            None => h,
        };
        if h.ty != union {
            return Some(self.error_expr(span));
        }
        let (def, _) = self.cx.union_def(union)?;
        let members = self.cx.union_members(union)?;
        let v = members.iter().position(|m| *m == target)? as u32;
        let consume = !is_place(&h) || outer_mode(&h) == Some(UseMode::Move);
        let mut mode = |t: TyId| match (self.cx.is_copy(t), consume) {
            (true, _) => UseMode::Copy,
            (false, true) => UseMode::Move,
            (false, false) => UseMode::Borrow,
        };
        let (tm, um) = (mode(target), mode(union));
        // The member `M`: its field.
        let m = tm;
        let b = self.new_local("<asserted>", target, false, span, LocalKind::Bind);
        let value = self.mk(H::Local(b, m), target, span);
        let field = self.member_of(value, prop, Want::Borrow, span);
        let ty = if test { any } else { field.ty };
        let field = match test {
            true => {
                let field = self.share_if_place(field);
                self.coerce(field, any)
            }
            false => field,
        };
        // Any other member: the property read through `any`.
        let m = um;
        let o = self.new_local("<value>", union, false, span, LocalKind::Bind);
        let other = self.mk(H::Local(o, m), union, span);
        let other = self.share_if_place(other);
        let boxed = self.coerce(other, any);
        let read = match self.dynamic_get(boxed, &prop.name, Want::Borrow, span) {
            Ok(h) | Err(h) => h,
        };
        let read = match test {
            true => read,
            false => self.cast_dynamic(read, ty),
        };
        let pat = |kind| hir::Pat {
            kind,
            ty: union,
            span,
        };
        let arms = vec![
            hir::Arm {
                pat: pat(P::Variant {
                    def,
                    variant: v,
                    args: vec![hir::Pat {
                        kind: P::Binding(b, tm),
                        ty: target,
                        span,
                    }],
                }),
                guard: None,
                body: field,
            },
            hir::Arm {
                pat: pat(P::Binding(o, um)),
                guard: None,
                body: read,
            },
        ];
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        Some(self.mk(kind, ty, span))
    }

    /// Does `(u as target).name` read as JavaScript does: `u` a union (outside generic code)
    /// with the object type `target`, which has the field `name`, as a member?
    fn cast_member_applies(&mut self, u: TyId, target: TyId, name: &str) -> bool {
        let generic = |s: &Self, t: TyId| {
            let mut ps = vec![];
            collect_params(&s.cx.ty, t, &mut ps);
            !ps.is_empty()
        };
        if self.cx.ty.has_error(u) || self.cx.ty.has_error(target) {
            return false;
        }
        if generic(self, u) || generic(self, target) || self.cx.union_def(u).is_none() {
            return false;
        }
        let member = self
            .cx
            .union_members(u)
            .is_some_and(|ms| ms.contains(&target));
        member && self.adt_of(target).is_some() && self.field_of(target, name).is_some()
    }
}
