//! `String(x)` and template literal parts of a value whose type `console.log` cannot print, but
//! JavaScript writes: an object type with a function, interface or `any` field, alone or in a
//! union (`raw: string | number | VNode | undefined`, issue #930). JavaScript's `String(x)`
//! of such an object is its `toString()` (`[object Object]` for a plain object); none of its
//! fields matter. The value is written as `String(x)` of an `any` writes it (`any_text.rs`),
//! which finds the member the union holds at run time. A function value (whose text is its
//! source in Node) and a value holding a lock (which an `any` cannot hold) stay errors.
//!
//! A value of an interface type (`Shape`, lib.dom's `Node`), alone, nullable or in a union, is
//! written and printed the same way: `console.log` of it shows the object it holds, as Node
//! shows that object (`Sq { kind: 'sq' }`, `{ kind: 'lit', area: [Function: area] }`).

use crate::body::FnCx;
use crate::hir::{self, DefId, TyId, TyKind};

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

    /// `h` of `console.log(h)`, whose type [`Self::printable`] refused, as an `any` when the
    /// parts of its type that aren't printable are interfaces (module docs); else `h` as it is.
    pub(crate) fn printed_iface_as_any(&mut self, h: hir::Expr) -> hir::Expr {
        if self.printable(h.ty) || self.iface_parts(h.ty, 0) != Some(true) {
            return h;
        }
        let Some(any) = self.any_ty() else {
            return h;
        };
        let h = self.share_if_place(h);
        self.try_coerce(h, any).unwrap_or_else(|h| h)
    }

    /// Is every member of `t` (through nullables and unions) an object type, a printable value
    /// or one holding interfaces, and `t` holds no lock?
    fn written_through_any(&mut self, t: TyId) -> bool {
        if self.may_hold_lock(t, &mut vec![]) {
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
            object || self.iface_parts(m, 0).is_some()
        })
    }

    /// Whether `t` is printable but for interface values in it: `None` when some other part
    /// isn't (a function, a lock), else whether it holds an interface (`Shape`, `Shape[]`,
    /// `{ s: Shape } | null`). `any` and `unknown` print as they are.
    fn iface_parts(&mut self, t: TyId, depth: u32) -> Option<bool> {
        if self.printable(t) {
            return Some(false);
        }
        if let TyKind::Dyn(d, _) = *self.cx.ty.kind(t) {
            if self.cx.dynamic(t).is_some() {
                return Some(false);
            }
            let refused = self.may_hold_lock(t, &mut vec![]) || self.literal_setter_impl(d);
            return (!refused).then_some(true);
        }
        if depth > 8 || self.cx.holds_lock(t) {
            return None;
        }
        let parts: Vec<TyId> = if let Some(p) = self.cx.ty.opt_payload(t) {
            vec![p]
        } else if let Some(members) = self.cx.union_members(t) {
            members
        } else {
            match self.cx.ty.kind(t).clone() {
                TyKind::Adt(d, args) => {
                    let fields: Vec<TyId> = self.cx.adt(d)?.fields.iter().map(|f| f.ty).collect();
                    fields
                        .into_iter()
                        .map(|f| self.cx.subst(f, &args))
                        .collect()
                }
                TyKind::FnPtr { .. }
                | TyKind::Closure(_)
                | TyKind::Dyn(..)
                | TyKind::Unit
                | TyKind::Simd(_)
                | TyKind::Shared(_) => return None,
                k => crate::types::children(&k),
            }
        };
        let mut found = false;
        for p in parts {
            found |= self.iface_parts(p, depth + 1)?;
        }
        Some(found)
    }

    /// Does a value of `t` reach a `Mutex` or `RwLock`, also through an interface value, by
    /// what the program's implementations of the interface hold? An `any` can't hold one.
    fn may_hold_lock(&mut self, t: TyId, seen: &mut Vec<TyId>) -> bool {
        if seen.contains(&t) || seen.len() > 64 || self.cx.dynamic(t).is_some() {
            return false;
        }
        seen.push(t);
        if self.cx.holds_lock(t) {
            return true;
        }
        let parts: Vec<TyId> = match self.cx.ty.kind(t).clone() {
            TyKind::Dyn(d, _) => (self.cx.impls.iter())
                .filter(|i| i.iface == d || self.cx.iface_extends_def(i.iface, d, 0))
                .map(|i| i.ty)
                .collect(),
            TyKind::Adt(d, args) => match self.cx.adt(d) {
                Some(a) => a.fields.iter().map(|f| f.ty).collect::<Vec<_>>(),
                None => vec![],
            }
            .into_iter()
            .map(|f| self.cx.subst(f, &args))
            .collect(),
            TyKind::FnPtr { .. } | TyKind::Closure(_) | TyKind::Param(_) => vec![],
            k => crate::types::children(&k),
        };
        parts.into_iter().any(|p| self.may_hold_lock(p, seen))
    }

    /// Does the program fill a field of interface `d` (or one extending it) with an object
    /// literal's `set f(v)`? Node prints such a property `[Getter/Setter]`, which the glue of a
    /// value held as the interface doesn't know yet: printing it stays refused.
    fn literal_setter_impl(&self, d: DefId) -> bool {
        (self.cx.impls.iter())
            .filter(|i| i.iface == d || self.cx.iface_extends_def(i.iface, d, 0))
            .any(|i| match self.cx.ty.kind(i.ty) {
                TyKind::Adt(s, _) if self.cx.literal_structs.contains(s) => (self.cx.adt(*s))
                    .is_some_and(|a| a.fields.iter().any(|f| f.name.starts_with("set "))),
                _ => false,
            })
    }
}
