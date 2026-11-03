//! `String(x)` and template literal parts of a value whose type `console.log` cannot print, but
//! JavaScript writes: an object type with a function, interface or `any` field, alone or in a
//! union (`raw: string | number | VNode | undefined`, issue #930). JavaScript's `String(x)`
//! of such an object is its `toString()` (`[object Object]` for a plain object); none of its
//! fields matter. The value is written as `String(x)` of an `any` writes it (`any_text.rs`),
//! which finds the member the union holds at run time. A function value (whose text is its
//! source in Node) and a value holding a lock (which an `any` cannot hold) stay errors.

use crate::body::FnCx;
use crate::hir::{self, TyId, TyKind};

impl FnCx<'_, '_> {
    /// The text of `h`, which [`Self::printable`] refused (`what` names the site in errors):
    /// through `any` when every member of its type is an object type or printable (module
    /// docs), else the "cannot format" error.
    pub(crate) fn unprintable_text(&mut self, h: hir::Expr, what: &str) -> hir::Expr {
        let (ty, span) = (h.ty, h.span);
        let any = self.any_ty().filter(|_| self.writes_dynamic_text());
        if let Some(any) = any.filter(|_| self.written_through_any(ty)) {
            let h = self.share_if_place(h);
            if let Ok(a) = self.try_coerce(h, any) {
                return self.dynamic_text(a, span);
            }
        }
        let tn = self.cx.display(ty);
        self.cx.err(
            format!("cannot format a value of type `{tn}` in {what}"),
            span,
        );
        self.error_expr(span)
    }

    /// Is every member of `t` (through nullables and unions) an object type or a printable
    /// value, and `t` holds no lock?
    fn written_through_any(&mut self, t: TyId) -> bool {
        if self.cx.holds_lock(t) {
            return false;
        }
        let mut inner = t;
        while let Some(p) = self.cx.ty.opt_payload(inner) {
            inner = p;
        }
        let members = self.cx.union_members(inner).unwrap_or_else(|| vec![inner]);
        members.into_iter().all(|m| {
            let object = matches!(self.cx.ty.kind(m), TyKind::Adt(..))
                && self.cx.union_def(m).is_none()
                && self.record_args(m).is_none();
            object || self.printable(m)
        })
    }
}
