//! Error values change type on their way to a handler: a callee's (or a `throw`'s) error type is
//! one member, or a subset, of the union the handler (a `catch` variable, the function's own
//! error type) holds (hir_encodings.md "Errors"). Widening is a move: tag the value with its
//! member's variant, re-tag a narrower union member by member, or upcast a class (a no-op).
//! Unions built by substituting into generic unions may nest; members are found recursively.

use velt_sema::hir::{TyId, TyKind};

use super::operand::proj;
use super::{cint, ice, unit, FnLower};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// Convert the owned error value `v` of concrete type `from` into type `to`; the result is
    /// owned by the caller (not registered as a temporary).
    pub(super) fn widen_error(&mut self, v: Operand, from: TyId, to: TyId) -> Operand {
        if from == to || self.dead() {
            return v;
        }
        if self.cx.is_union(to) {
            if let Some(k) = self.member_variant(to, from) {
                let payload_ty = self.cx.variant_tys(to, k)[0];
                let p = self.widen_error(v, from, payload_ty);
                return self.union_value(to, k, p, payload_ty);
            }
        }
        if self.cx.is_union(from) {
            return self.retag(v, from, to);
        }
        if self.cx.is_class(from) && self.cx.is_class(to) {
            return v;
        }
        ice(format_args!(
            "cannot convert error `{}` to `{}`",
            self.cx.type_name(from),
            self.cx.type_name(to)
        ))
    }

    /// The variant of union `u` that can hold an error of type `m`: `m` itself, a union member
    /// containing it, or a base class of it.
    fn member_variant(&mut self, u: TyId, m: TyId) -> Option<u32> {
        let n = self.union_len(u);
        let members: Vec<TyId> = (0..n).map(|k| self.cx.variant_tys(u, k)[0]).collect();
        if let Some(k) = members.iter().position(|&p| p == m) {
            return Some(k as u32);
        }
        if let Some(k) = members
            .iter()
            .position(|&p| self.cx.is_union(p) && self.member_variant(p, m).is_some())
        {
            return Some(k as u32);
        }
        if self.cx.is_union(m) {
            return None;
        }
        let bases = self.cx.bases_of(m);
        members
            .iter()
            .position(|p| bases.contains(p))
            .map(|k| k as u32)
    }

    fn union_len(&mut self, u: TyId) -> u32 {
        match self.cx.kind(u) {
            TyKind::Adt(d, _) => self.cx.enum_def(d).variants.len() as u32,
            _ => ice("union length of a non-union"),
        }
    }

    /// Union value of type `u` in variant `k` holding `payload` (of type `payload_ty`).
    fn union_value(&mut self, u: TyId, k: u32, payload: Operand, payload_ty: TyId) -> Operand {
        let base = self.cx.ty(u);
        let view = self.cx.view(u, k);
        let t = self.temp(base);
        let vp = proj(&Place::local(t), Proj::Cast(view));
        self.assign(
            proj(&vp, Proj::Field(0)),
            Rvalue::Use(cint(k as i128, Ty::U32)),
        );
        if self.cx.ty(payload_ty) != Ty::Unit {
            self.assign(proj(&vp, Proj::Field(1)), Rvalue::Use(payload));
        }
        Operand::Copy(Place::local(t))
    }

    /// Convert a union error member by member (`from` narrower than or nested in `to`).
    fn retag(&mut self, v: Operand, from: TyId, to: TyId) -> Operand {
        let vt = self.cx.ty(from);
        let src = self.copy_to_temp(v, vt);
        let dt = self.cx.ty(to);
        let dst = self.temp(dt);
        let join = self.new_block();
        self.for_each_member(&Place::local(src), from, |lw, payload, m| {
            let w = lw.widen_error(payload, m, to);
            lw.assign(Place::local(dst), Rvalue::Use(w));
            lw.goto(join);
        });
        self.switch_to(join);
        Operand::Copy(Place::local(dst))
    }

    /// Branch on the tag of the union value at `p` (of concrete type `u`) and run `each` with
    /// the payload and its type in every member's block (members that cannot hold a value,
    /// such as `never`, get an unreachable block). Leaves the builder in an unreachable block.
    pub(super) fn for_each_member(
        &mut self,
        p: &Place,
        u: TyId,
        mut each: impl FnMut(&mut Self, Operand, TyId),
    ) {
        let n = self.union_len(u);
        let tag = self.rvalue_temp(Ty::U32, Rvalue::Use(Operand::Copy(proj(p, Proj::Field(0)))));
        for k in 0..n {
            let m = self.cx.variant_tys(u, k)[0];
            let (this, next) = (self.new_block(), self.new_block());
            let is_k = self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(BinOp::Eq, tag.clone(), cint(k as i128, Ty::U32)),
            );
            self.branch(is_k, this, next);
            self.switch_to(this);
            if self.cx.is_never(m) {
                self.terminate(crate::vir::Terminator::Unreachable);
            } else {
                let payload = match self.cx.ty(m) {
                    Ty::Unit => unit(),
                    _ => {
                        let view = self.cx.view(u, k);
                        Operand::Copy(proj(&proj(p, Proj::Cast(view)), Proj::Field(1)))
                    }
                };
                each(self, payload, m);
            }
            self.switch_to(next);
        }
        self.terminate(crate::vir::Terminator::Unreachable);
    }
}
