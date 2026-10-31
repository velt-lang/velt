//! Type parameters bounded by a class (`T extends Base`, `E extends Error`), as in TypeScript: a
//! value of `T` has the class's fields, getters and methods. Read as a member access's receiver,
//! it is the value upcast to the class (`Upcast`, a no-op on the pointer), so the member resolves
//! as on a `Base`, and each instantiation reads it from its own class (`Sub`, whose layout starts
//! with `Base`'s fields; an overridden method runs through the vtable). A subclass satisfies the
//! bound (`type_level::relate`).

use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// The class bounding type parameter `t` (`Base` for `T extends Base`), unless `t` also has
    /// interface bounds, whose members are read through them. A `Record<K, V>` bound is an
    /// object type's, read by key (`generic_member`).
    pub(crate) fn class_bound(&mut self, t: TyId) -> Option<TyId> {
        let TyKind::Param(p) = *self.cx.ty.kind(t) else {
            return None;
        };
        if self.bounds.get(p as usize).is_some_and(|b| !b.is_empty()) {
            return None;
        }
        let c = self.cx.constraint_of(p)?;
        self.cx.class_of(c)?;
        self.record_args(c).is_none().then_some(c)
    }

    /// The receiver `h` as its class bound (`class_bound`), else as it is.
    pub(crate) fn as_class_bound(&mut self, h: hir::Expr) -> hir::Expr {
        match self.class_bound(h.ty) {
            Some(c) => {
                let span = h.span;
                self.mk(H::Upcast(Box::new(h)), c, span)
            }
            None => h,
        }
    }

    /// A value of a type parameter bounded by a class where that class, a class it extends or
    /// an interface it implements is expected (`show(x)` with `show(b: Base)`): the value as the
    /// class, converted. `Err(h)` when it is not such a value or the class does not convert.
    pub(super) fn class_bound_value(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        if self.class_bound(h.ty).is_none() || matches!(self.cx.ty.kind(exp), TyKind::Param(_)) {
            return Err(h);
        }
        let before = self.cx.diags.len();
        let up = self.as_class_bound(h.clone());
        match self.try_coerce(up, exp) {
            Ok(c) if self.cx.diags.len() == before => Ok(c),
            _ => {
                self.cx.diags.truncate(before);
                Err(h)
            }
        }
    }
}
