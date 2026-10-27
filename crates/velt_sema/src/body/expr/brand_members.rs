//! Members of a class instance brand's object part (`crate::brands`): `s.$set` on an
//! `s: Map<K, V> & { $set(v: Map<K, V>): void }`, the type of sigx's `signal(new Map())`. The
//! value is the class instance itself (an `as` cast keeps it, as in TypeScript), whose class
//! does not declare the member, so it is read from the object at run time, as through an `any`,
//! and checked as the member's type then: a reactive proxy's `get` trap provides it, and a
//! plain instance lacks it (a `TypeError` at the read, where Node reads `undefined`). A call
//! `s.$set(v)` calls the function read. The class's own members are the class's (`unbrand`).

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, Intrinsic, TyId};

impl FnCx<'_, '_> {
    /// The type of member `name` of `t`'s object part, when `t` brands a class instance whose
    /// class has no member `name` (module docs).
    fn brand_tag_member_ty(&mut self, t: TyId, name: &str) -> Option<TyId> {
        let base = self.cx.brand_base(t)?;
        self.cx.class_of(base)?;
        let tag = self.cx.brand_tag(t)?;
        let f = (self.cx.public_fields(tag).into_iter()).find(|f| f.name == name)?;
        if self.field_of(base, name).is_some()
            || self.has_getter(base, name)
            || self.method_exists(base, name)
        {
            return None;
        }
        Some(match f.optional && self.cx.dynamic(f.ty).is_none() {
            true => self.cx.ty.option_of(f.ty, hir::Nothing::Undefined),
            false => f.ty,
        })
    }

    /// `obj.name` read on a class instance brand, `name` a member of its object part only:
    /// read from the object at run time (module docs). `Err(obj)` otherwise.
    pub(super) fn brand_tag_read(
        &mut self,
        obj: hir::Expr,
        name: &str,
        want: Want,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        if want == Want::BorrowMut {
            return Err(obj);
        }
        let Some(ty) = self.brand_tag_member_ty(obj.ty, name) else {
            return Err(obj);
        };
        let Some(any) = self.any_ty() else {
            return Err(obj);
        };
        let obj = self.unbrand(obj);
        let obj = self.share_if_place(obj);
        let boxed = match self.box_dynamic(obj, any) {
            Ok(h) | Err(h) => h,
        };
        let read = match self.dynamic_get(boxed, name, Want::Borrow, span) {
            Ok(h) | Err(h) => h,
        };
        if self.cx.ty.is_bottom(read.ty) {
            return Ok(read);
        }
        Ok(self.intrinsic(Intrinsic::AnyTo, vec![read], ty, span))
    }

    /// `obj.name` read on an `any` (`dynamic_get`) or a class instance brand (`brand_tag_read`).
    pub(super) fn dynamic_or_brand_get(
        &mut self,
        obj: hir::Expr,
        name: &str,
        want: Want,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        match self.dynamic_get(obj, name, want, span) {
            Ok(h) => Ok(h),
            Err(obj) => self.brand_tag_read(obj, name, want, span),
        }
    }

    /// `obj.name(args)` on a class instance brand, `name` a member of its object part only: a
    /// call of the function read (`brand_tag_read`). `Err(obj)` otherwise.
    pub(super) fn brand_tag_call(
        &mut self,
        obj: hir::Expr,
        prop: &ast::Ident,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let f = self.brand_tag_read(obj, &prop.name, Want::Borrow, prop.span)?;
        Ok(self.call_value(f, args, span))
    }
}
