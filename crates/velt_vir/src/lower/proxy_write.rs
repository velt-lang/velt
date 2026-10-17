//! Writes through typed proxies (#823, proxy_typed.rs): `s.f = v`, `s.f op= v`, and the update
//! and assignment blocks sema writes for `s.f++`, `++s.f` and `(s.f = v)` used as values, which
//! run the `get` trap at most once, as in JavaScript.

use velt_sema::hir::{self, TyId};

use super::{cint, FnLower, Glue, Work};
use crate::vir::{Callee, Operand, Place, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// The base of a write to a field of the proxiable `bty`, held (shared) for the statement
    /// when the right-hand side `value` may write memory (and so drop the object), and always
    /// when it holds a proxy: a trap is user code, which may drop the last other reference
    /// (a `get` trap reassigning the variable `s.f += 1` read). The hold is on the cold path.
    fn proxy_hold(&mut self, base: &hir::Expr, bty: TyId, value: &hir::Expr) -> Operand {
        let v = self.expr(base);
        if self.may_write(value) {
            let s = self.share_value(v, bty);
            let o = self.own_value(s, bty);
            return self.rvalue_temp(Ty::Ptr, Rvalue::Use(o));
        }
        let v = self.rvalue_temp(Ty::Ptr, Rvalue::Use(v));
        let slot = self.temp(Ty::Ptr);
        self.assign(Place::local(slot), Rvalue::Use(cint(0, Ty::Ptr)));
        self.own_temp(slot, bty);
        self.proxy_split(
            &v,
            |lw, u| {
                lw.retain(u);
                lw.assign(Place::local(slot), Rvalue::Use(v.clone()));
            },
            |_| {},
        );
        v
    }

    /// Store the owned `v` into field `index` of the `bty` value at `ptr`, through the `set`
    /// trap when it holds a proxy. `place` is the HIR target (for a presence flag).
    fn proxy_write_at(
        &mut self,
        ptr: &Operand,
        bty: TyId,
        index: u32,
        place: &hir::Expr,
        v: Operand,
    ) {
        let (name, fty) = self.proxy_field(bty, index);
        let fvt = self.cx.ty(fty);
        let val = self.copy_to_temp(v, fvt);
        let drops = self.cx.needs_drop(fty);
        self.proxy_split(
            ptr,
            |lw, u| {
                if name.starts_with('#') {
                    return lw.proxy_private(&name);
                }
                let key = lw.proxy_key_addr(&name);
                let vp = lw.addr(Place::local(val));
                let f = lw.cx.func(Work::Glue(Glue::ProxySet, fty));
                lw.call(Callee::Func(f), vec![u, key, vp], None, false);
            },
            |lw| {
                let fp = lw.proxy_hot_place(ptr, bty, index);
                let v = Operand::Copy(Place::local(val));
                lw.store_assigned(place, fp, v, fty, true, drops);
            },
        );
    }

    /// `base.f = value` on the proxiable `bty`.
    pub(super) fn proxy_assign(
        &mut self,
        place: &hir::Expr,
        base: &hir::Expr,
        index: u32,
        value: &hir::Expr,
    ) -> Operand {
        self.proxy_assign_as(place, (base, index), value, false)
    }

    /// [`proxy_assign`](Self::proxy_assign), also giving the assigned value when `want` (the
    /// assignment used as a value: JavaScript's value is the right-hand side, with no `get`).
    fn proxy_assign_as(
        &mut self,
        place: &hir::Expr,
        (base, index): (&hir::Expr, u32),
        value: &hir::Expr,
        want: bool,
    ) -> Operand {
        let bty = self.sub(base.ty);
        let ptr = self.proxy_hold(base, bty, value);
        let v = self.consume(value);
        if self.dead() {
            return super::unit();
        }
        let v = self.detach(v, place.ty);
        let fty = self.sub(place.ty);
        let out = match (want, self.cx.needs_drop(fty)) {
            (false, _) => super::unit(),
            (true, false) => {
                let fvt = self.cx.ty(fty);
                Operand::Copy(Place::local(self.copy_to_temp(v.clone(), fvt)))
            }
            (true, true) => {
                let s = self.share_value(v.clone(), fty);
                self.own_value(s, fty)
            }
        };
        self.proxy_write_at(&ptr, bty, index, place, v);
        out
    }

    /// `base.f op= value` on the proxiable `bty`: the read, the right-hand side, the write.
    pub(super) fn proxy_compound(
        &mut self,
        op: hir::BinOp,
        place: &hir::Expr,
        (base, index): (&hir::Expr, u32),
        value: &hir::Expr,
    ) -> Operand {
        self.proxy_update(op, place, (base, index), value, None);
        super::unit()
    }

    /// [`proxy_compound`](Self::proxy_compound), also giving the field's old value
    /// (`want_old`, a postfix update used as a value) or its new one (a prefix update): the
    /// `get` trap runs once, as in JavaScript. `None` gives unit.
    fn proxy_update(
        &mut self,
        op: hir::BinOp,
        place: &hir::Expr,
        (base, index): (&hir::Expr, u32),
        value: &hir::Expr,
        want_old: Option<bool>,
    ) -> Operand {
        let bty = self.sub(base.ty);
        let ptr = self.proxy_hold(base, bty, value);
        let (_, fty) = self.proxy_field(bty, index);
        let old = self.proxy_read_at(&ptr, bty, index);
        let fvt = self.cx.ty(fty);
        let old = self.copy_to_temp(Operand::Copy(old), fvt);
        let r = self.expr(value);
        if self.dead() {
            return super::unit();
        }
        let v = self.binop(op, Operand::Copy(Place::local(old)), r, place.ty, place.ty);
        let v = match self.cx.needs_drop(fty) {
            true => self.take_owned(v),
            false => v,
        };
        let out = match want_old {
            Some(true) => Operand::Copy(Place::local(old)),
            Some(false) => Operand::Copy(Place::local(self.copy_to_temp(v.clone(), fvt))),
            None => super::unit(),
        };
        self.proxy_write_at(&ptr, bty, index, place, v);
        out
    }

    /// `s.f++` / `++s.f` used as a value on the proxiable type of `s`, which sema writes as
    /// `{ let tmp = s.f; s.f += 1; tmp }` / `{ s.f += 1; s.f }`: one `get` trap, as in
    /// JavaScript, instead of a second read; and `(s.f = v)` / `(s.f op= v)` used as a value,
    /// `{ s.f = v; s.f }`: the value is `v` (or the computed one), without a read. `None` when
    /// `b` is not such a block.
    pub(super) fn proxy_update_block(&mut self, b: &hir::Block) -> Option<Operand> {
        if !self.cx.has_proxies() {
            return None;
        }
        let value = b.value.as_deref()?;
        let (ca, old) = match b.stmts.as_slice() {
            [hir::Stmt {
                kind:
                    hir::StmtKind::Let {
                        local,
                        init: Some(read),
                    },
                ..
            }, hir::Stmt {
                kind: hir::StmtKind::Expr(ca),
                ..
            }] => {
                let hir::ExprKind::Local(l, _) = &value.kind else {
                    return None;
                };
                (l == local).then_some(())?;
                (ca, read)
            }
            [hir::Stmt {
                kind: hir::StmtKind::Expr(ca),
                ..
            }] => (ca, value),
            _ => return None,
        };
        let want_old = !std::ptr::eq(old, value);
        if let hir::ExprKind::Assign { place, value: rhs } = &ca.kind {
            let hir::ExprKind::Field { base, index, .. } = &place.kind else {
                return None;
            };
            let bty = self.sub(base.ty);
            if want_old
                || !same_place(place, value)
                || !self.cx.proxiable(bty)
                || self.is_unit_field(bty, *index)
            {
                return None;
            }
            return Some(self.proxy_assign_as(place, (base, *index), rhs, true));
        }
        let hir::ExprKind::CompoundAssign {
            op,
            place,
            value: one,
        } = &ca.kind
        else {
            return None;
        };
        let hir::ExprKind::Field { base, index, .. } = &place.kind else {
            return None;
        };
        let bty = self.sub(base.ty);
        let fty = self.sub(place.ty);
        // A postfix update's old value is its `let`; a prefix one (or `(s.f op= v)`) gives the
        // new value, whatever the operand.
        let simple = match want_old {
            true => {
                matches!(one.kind, hir::ExprKind::Lit(_))
                    && matches!(op, hir::BinOp::Add | hir::BinOp::Sub)
            }
            false => true,
        } && same_place(place, old);
        if !simple
            || !self.cx.proxiable(bty)
            || self.is_unit_field(bty, *index)
            || self.cx.needs_drop(fty)
        {
            return None;
        }
        Some(self.proxy_update(*op, place, (base, *index), one, Some(want_old)))
    }
}

/// Do `a` and `b` name the same place (locals, globals, and fields, elements and narrowed
/// payloads of them, with indices that are locals or literals, as sema hoists them for an
/// update), so reading one reads the other? Conservative: `false` for anything else.
fn same_place(a: &hir::Expr, b: &hir::Expr) -> bool {
    use hir::ExprKind as K;
    match (&a.kind, &b.kind) {
        (K::Local(x, _), K::Local(y, _)) => x == y,
        (K::Global(x), K::Global(y)) => x == y,
        (
            K::Field {
                base: x, index: i, ..
            },
            K::Field {
                base: y, index: j, ..
            },
        ) => i == j && same_place(x, y),
        (
            K::Index {
                base: x, index: i, ..
            },
            K::Index {
                base: y, index: j, ..
            },
        ) => same_index(i, j) && same_place(x, y),
        (K::UnwrapSome(x, _), K::UnwrapSome(y, _)) | (K::Downcast(x), K::Downcast(y)) => {
            same_place(x, y)
        }
        (
            K::UnwrapVariant {
                expr: x,
                variant: i,
                ..
            },
            K::UnwrapVariant {
                expr: y,
                variant: j,
                ..
            },
        ) => i == j && same_place(x, y),
        _ => false,
    }
}

/// Are `a` and `b` the same index: one local, or equal literals?
fn same_index(a: &hir::Expr, b: &hir::Expr) -> bool {
    use hir::ExprKind as K;
    match (&a.kind, &b.kind) {
        (K::Local(x, _), K::Local(y, _)) => x == y,
        (K::Lit(x), K::Lit(y)) => x == y && a.ty == b.ty,
        _ => false,
    }
}
