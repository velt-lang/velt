//! Members and indexes of values whose type is known only per instantiation, read through
//! their bound or apparent type (issue #1212, #350): `xs[0]` with `T extends readonly number[]`,
//! `p.name` with `p: Partial<T>`, `{} as Pick<T, K>`.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `obj[index]` read where `obj`'s type is known only per instantiation and the key is an
    /// ordinary one (`xs[0]` with `T extends readonly number[]`, `o[k]` with `T extends
    /// Record<string, V>` and `k: string`; issue #1212): the member of the value boxed as an
    /// `any`, converted to the element type of `T`'s bound when it is an array or a record
    /// (checked at run time), else an `any`. A key whose type is a key of `T` known only per
    /// instantiation is `key_index.rs`'s. `Err(obj)` for anything else.
    pub(super) fn generic_index(
        &mut self,
        obj: hir::Expr,
        index: &ast::Expr,
        want: Want,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        if want == Want::BorrowMut || !self.per_instance(obj.ty) {
            return Err(obj);
        }
        // `o[p as keyof T]` reads the key `p` holds: the assertion converts nothing.
        let index = match &index.kind {
            ast::ExprKind::Cast { expr, ty } if matches!(ty.kind, ast::TypeExprKind::KeyOf(_)) => {
                &**expr
            }
            _ => index,
        };
        // The key's type must be known, and not a key of `T` known only per instantiation.
        let literal = matches!(&index.kind, ast::ExprKind::Lit(_));
        let generic_key = !literal
            && self.peek_ty(index).is_none_or(|k| {
                matches!(
                    self.cx.ty.kind(k),
                    TyKind::Param(_) | TyKind::Op(hir::TyOp::Inline(_), _)
                )
            });
        // A string key of a parameter with interface bounds names a member of theirs, read
        // through them (`o["name"]` as `o.name`).
        let bounded = match self.cx.ty.kind(obj.ty) {
            TyKind::Param(p) => self.bounds.get(*p as usize).is_some_and(|b| !b.is_empty()),
            _ => false,
        };
        let named = matches!(&index.kind, ast::ExprKind::Lit(ast::Lit::Str(_)));
        if generic_key || (named && bounded) {
            return Err(obj);
        }
        let elem = match self.cx.ty.kind(obj.ty) {
            TyKind::Param(p) => self
                .cx
                .constraint_of(*p)
                .and_then(|c| match self.cx.ty.kind(c) {
                    TyKind::Array(e) => Some(*e),
                    _ => self.record_args(c).map(|(_, v)| v),
                }),
            _ => None,
        };
        let boxed = self.generic_as_any(obj)?;
        let read = match self.dynamic_index(boxed, index, want, span) {
            Ok(h) => h,
            Err(_) => return Ok(self.error_expr(span)),
        };
        Ok(match elem.filter(|e| self.cx.dynamic(*e).is_none()) {
            Some(e) => self.intrinsic(hir::Intrinsic::AnyTo, vec![read], e, span),
            None => read,
        })
    }

    /// `x.name` read where `x`'s type is a type parameter bounded by a type that is no
    /// interface or object type (`T extends readonly number[]`, `T extends string`; issue
    /// #1212): the member of the value boxed as an `any` (an `any`; `length` of an array or
    /// string bound is a number). `Err(obj)` for anything else.
    pub(super) fn bound_member(
        &mut self,
        obj: hir::Expr,
        name: &str,
        want: Want,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let TyKind::Param(p) = *self.cx.ty.kind(obj.ty) else {
            return Err(obj);
        };
        let interface_bound = self.bounds.get(p as usize).is_some_and(|b| !b.is_empty());
        let Some(c) = self.cx.constraint_of(p).filter(|_| !interface_bound) else {
            return Err(obj);
        };
        if want == Want::BorrowMut || matches!(self.cx.ty.kind(c), TyKind::FnPtr { .. }) {
            return Err(obj);
        }
        let length = name == "length"
            && (matches!(self.cx.ty.kind(c), TyKind::Array(_)) || c == self.cx.ty.str_);
        let boxed = self.generic_as_any(obj)?;
        let read = match self.dynamic_get(boxed, name, want, span) {
            Ok(h) => h,
            Err(_) => return Ok(self.error_expr(span)),
        };
        Ok(match length {
            true => {
                let f64_ = self.cx.ty.f64;
                self.intrinsic(hir::Intrinsic::AnyTo, vec![read], f64_, span)
            }
            false => read,
        })
    }

    /// `p.name` read where `p`'s type is a deferred form over type parameters bounded by
    /// object types (`p: Partial<T>`, `T extends { name: string }`; #350): the field has the
    /// type the form gives at the bounds (`string | undefined`), and is read from the value
    /// boxed as an `any`, checked. `Err(obj)` for anything else.
    pub(super) fn form_member(
        &mut self,
        obj: hir::Expr,
        name: &str,
        want: Want,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let TyKind::Op(hir::TyOp::Inline(_) | hir::TyOp::Alias(_), _) = self.cx.ty.kind(obj.ty)
        else {
            return Err(obj);
        };
        if want == Want::BorrowMut || !self.cx.has_type_params(obj.ty) {
            return Err(obj);
        }
        // Each type parameter at its object bound, the others as they are.
        let n = self.bounds.len() as u32;
        let at_bounds: Vec<TyId> = (0..n)
            .map(|p| {
                self.cx
                    .constraint_of(p)
                    .filter(|&c| self.cx.is_object_type(c) && !self.cx.has_type_params(c))
                    .unwrap_or_else(|| self.cx.ty.param(p))
            })
            .collect();
        let at = self.cx.subst(obj.ty, &at_bounds);
        if !self.cx.is_object_type(at) {
            return Err(obj);
        }
        let Some(f) = self
            .cx
            .public_fields(at)
            .into_iter()
            .find(|f| f.name == name)
        else {
            return Err(obj);
        };
        let fty = match f.optional && self.cx.dynamic(f.ty).is_none() {
            true => self.cx.ty.option_of(f.ty, hir::Nothing::Undefined),
            false => f.ty,
        };
        let boxed = self.generic_as_any(obj)?;
        let read = match self.dynamic_get(boxed, name, want, span) {
            Ok(h) => h,
            Err(_) => return Ok(self.error_expr(span)),
        };
        Ok(self.intrinsic(hir::Intrinsic::AnyTo, vec![read], fty, span))
    }

    /// `{} as D` where `D` is an object type known only per instantiation (`Pick<T, K>` in the
    /// standard `pick` body; docs/internals/design/deferred-types.md, decision 5): a new empty
    /// record of `any` values, as an `any`. Its keys are set through it (`r[k] = o[k]`), and
    /// converting it to `D` (`return r`) builds `D` from its entries at each instantiation,
    /// each value checked, a missing required key a `TypeError` naming it. `None` for any
    /// other assertion.
    pub(super) fn empty_as_deferred(
        &mut self,
        expr: &ast::Expr,
        target: TyId,
        span: Span,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Object(props) = &super::object_builtins::strip_parens(expr).kind else {
            return None;
        };
        let deferred = matches!(
            self.cx.ty.kind(target),
            TyKind::Op(hir::TyOp::Inline(_) | hir::TyOp::Alias(_), _)
        ) && self.cx.has_type_params(target);
        if !props.is_empty() || !deferred {
            return None;
        }
        let any = self.any_ty()?;
        let record = self.cx.prelude_adt("Record")?;
        let str_ = self.cx.ty.str_;
        let rty = self.cx.ty.intern(TyKind::Adt(record, vec![str_, any]));
        self.cx.objects.record_fills = true;
        let r = self.expr(expr, Some(rty), Want::Move);
        let r = self.coerce(r, rty);
        let _ = span;
        Some(self.coerce(r, any))
    }
}
