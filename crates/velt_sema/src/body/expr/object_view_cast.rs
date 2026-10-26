//! `o as T` of an object whose type has accessors (a literal with `get value()`, which is the
//! value of its own interface, `literal_iface.rs`) to an object type `T` that declares only
//! fields (#1510): TypeScript keeps the one object, so reading `T`'s field runs the getter each
//! time and writing it runs the setter. A copy would freeze the getter's value, so in a program
//! with typed proxies (#823), where an object type can hold a view, the cast is the conversion
//! through `any` a view comes from (velt_vir lower/any_view.rs): `AnyTo(T, o as any)`. Its field
//! reads and writes then go to the object's members (the getter, the setter) at that moment.
//! Without proxies the cast stays the error it was.

use velt_common::Span;

use crate::body::FnCx;
use crate::hir::{self, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `h as target` for an interface value `h` with a getter (module docs) and a fields-only
    /// object type `target` each of whose fields `h`'s interface has, as a field or a getter of
    /// a comparable type: a view of `h`'s object. `Err(h)` otherwise.
    pub(super) fn object_view_cast(
        &mut self,
        h: hir::Expr,
        target: TyId,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        if !self.cx.is_method_iface(h.ty) || self.cx.class_of(target).is_some() {
            return Err(h);
        }
        let TyKind::Adt(td, targs) = self.cx.ty.kind(target).clone() else {
            return Err(h);
        };
        let TyKind::Dyn(id, iargs) = self.cx.ty.kind(h.ty).clone() else {
            return Err(h);
        };
        if self.record_args(target).is_some() || !self.may_be_typed_proxy(target) {
            return Err(h);
        }
        let Some(adt) = self.cx.adt(td) else {
            return Err(h);
        };
        let wanted: Vec<(String, TyId)> =
            adt.fields.iter().map(|f| (f.name.clone(), f.ty)).collect();
        let Some(iface) = self.cx.iface(id) else {
            return Err(h);
        };
        let fields: Vec<(String, TyId)> = iface
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty))
            .collect();
        let getters: Vec<(String, TyId)> = (iface.methods.iter())
            .filter(|m| m.is_getter)
            .map(|m| (m.name.clone(), m.ret))
            .collect();
        if getters.is_empty() || wanted.is_empty() {
            return Err(h);
        }
        for (name, ft) in wanted {
            let ft = self.cx.subst(ft, &targs);
            let have = fields.iter().chain(&getters).find(|(n, _)| *n == name);
            let Some((_, ht)) = have else {
                return Err(h);
            };
            let ht = self.cx.subst(*ht, &iargs);
            if !self.view_member_comparable(ht, ft, span) {
                return Err(h);
            }
        }
        let Some(any) = self.any_ty() else {
            return Err(h);
        };
        let orig = h.clone();
        let Ok(a) = self.try_coerce(h, any) else {
            return Err(orig);
        };
        self.coerce_dynamic(a, target, true).map_err(|_| orig)
    }

    /// Does a value of `a` convert to `b`, or one of `b` to `a` (a member and the field it is
    /// seen as)?
    fn view_member_comparable(&mut self, a: TyId, b: TyId, span: Span) -> bool {
        if a == b {
            return true;
        }
        let probe = |lw: &mut Self, from: TyId, to: TyId| {
            let mut d = lw.error_expr(span);
            d.ty = from;
            lw.try_coerce(d, to).is_ok()
        };
        // A literal type compares as its base (`[BRAND]: true as const` and `[BRAND]: true`).
        let (a, b) = (self.cx.widened(a), self.cx.widened(b));
        probe(self, a, b) || probe(self, b, a)
    }
}
