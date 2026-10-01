//! Structural equality (`Intrinsic::Eq`, `==` on non-scalar types) and hashing
//! (`Intrinsic::Hash`). Class objects, interface values and function values compare and hash by
//! identity (JS reference semantics); everything else structurally. Hashes combine parts
//! FxHash-style: `h = (rotl(h, 5) ^ part) * 0x517cc1b727220a95`; strings hash their bytes with
//! `velt_rt_str_hash`.

use velt_sema::hir::{TyId, TyKind};

use super::Glue;
use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cint, FnLower};
use crate::vir::{self, BinOp, Const, Operand, Place, Proj, Rvalue, Terminator, Ty};

const FX_K: i128 = 0x517c_c1b7_2722_0a95;

impl FnLower<'_, '_> {
    /// `a == b` for values of concrete type `ty` at two places (Bool operand).
    pub(in crate::lower) fn eq_values(&mut self, a: &Place, b: &Place, ty: TyId) -> Operand {
        let vt = self.cx.ty(ty);
        match self.cx.kind(ty) {
            TyKind::Str => {
                let (pa, pb) = (self.addr(a.clone()), self.addr(b.clone()));
                self.str_eq(pa, pb)
            }
            TyKind::Unit | TyKind::Never | TyKind::Literal(_) => {
                Operand::Const(Const::Bool(true), Ty::Bool)
            }
            TyKind::FnPtr { .. } | TyKind::Closure(_) | TyKind::Dyn(..) => {
                let f = Proj::Field(0);
                let (x, y) = (Operand::Copy(proj(a, f.clone())), Operand::Copy(proj(b, f)));
                self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, x, y))
            }
            _ if vt.is_scalar() => {
                let (x, y) = (Operand::Copy(a.clone()), Operand::Copy(b.clone()));
                self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, x, y))
            }
            _ => {
                let (pa, pb) = (self.addr(a.clone()), self.addr(b.clone()));
                self.call_glue(Glue::Eq, ty, vec![pa, pb])
            }
        }
    }

    /// Hash of the value of concrete type `ty` at `place` (U64 operand).
    pub(in crate::lower) fn hash_value(&mut self, place: &Place, ty: TyId) -> Operand {
        let vt = self.cx.ty(ty);
        match self.cx.kind(ty) {
            TyKind::Str => {
                let a = self.addr(place.clone());
                let r = self.temp(Ty::U64);
                self.call_rt(Rt::StrHash, vec![a], Some(Place::local(r)));
                Operand::Copy(Place::local(r))
            }
            TyKind::Unit | TyKind::Never | TyKind::Literal(_) => cint(0, Ty::U64),
            TyKind::FnPtr { .. } | TyKind::Closure(_) | TyKind::Dyn(..) => {
                let p = Operand::Copy(proj(place, Proj::Field(0)));
                let x = self.cast_to(p, Ty::Ptr, Ty::U64);
                self.fx_combine(cint(0, Ty::U64), x)
            }
            _ if vt.is_float() => {
                let x = self.float_bits(Operand::Copy(place.clone()), vt);
                self.fx_combine(cint(0, Ty::U64), x)
            }
            _ if vt.is_scalar() => {
                let x = self.cast_to(Operand::Copy(place.clone()), vt, Ty::U64);
                self.fx_combine(cint(0, Ty::U64), x)
            }
            _ => {
                let a = self.addr(place.clone());
                self.call_glue(Glue::Hash, ty, vec![a])
            }
        }
    }

    /// `(rotl(h, 5) ^ x) * K`.
    fn fx_combine(&mut self, h: Operand, x: Operand) -> Operand {
        let l = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Shl, h.clone(), cint(5, Ty::U64)),
        );
        let r = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::UShr, h, cint(59, Ty::U64)));
        let rot = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::BitOr, l, r));
        let mix = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::BitXor, rot, x));
        self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Mul, mix, cint(FX_K as u64 as i128, Ty::U64)),
        )
    }

    /// Raw bits of a float (through memory: VIR casts are numeric conversions).
    fn float_bits(&mut self, v: Operand, vt: Ty) -> Operand {
        let (fa, ia) = if vt == Ty::F32 {
            (
                self.cx.new_agg("f32 bits".into(), &[Ty::F32]),
                self.cx.new_agg("u32 bits".into(), &[Ty::U32]),
            )
        } else {
            (
                self.cx.new_agg("f64 bits".into(), &[Ty::F64]),
                self.cx.new_agg("u64 bits".into(), &[Ty::U64]),
            )
        };
        let t = self.temp(Ty::Agg(fa));
        self.assign(Place::local(t), Rvalue::Aggregate(fa, vec![v]));
        let it = if vt == Ty::F32 { Ty::U32 } else { Ty::U64 };
        let bits = Operand::Copy(proj(
            &proj(&Place::local(t), Proj::Cast(ia)),
            Proj::Field(0),
        ));
        self.cast_to(bits, it, Ty::U64)
    }

    /// Parts compared/hashed structurally: (place in `a`, place in `b`, type) per stored field
    /// (`void` fields are always equal).
    fn struct_parts(&mut self, a: &Place, b: &Place, ty: TyId) -> Vec<(Place, Place, TyId)> {
        let tys = self.cx.part_types(ty);
        let mut out = vec![];
        for (i, t) in tys.into_iter().enumerate() {
            if !self.cx.is_unit(t) {
                let (x, y) = (
                    self.field_place(a, ty, i as u32),
                    self.field_place(b, ty, i as u32),
                );
                out.push((x, y, t));
            }
        }
        out
    }

    pub(super) fn eq_body(&mut self, pa: vir::Local, pb: vir::Local, ty: TyId) {
        let a = self.deref_param(pa, ty);
        let b = self.deref_param(pb, ty);
        let no = self.new_block();
        match self.cx.kind(ty) {
            TyKind::Adt(..) | TyKind::Tuple(_) if !self.is_enum(ty) => {
                for (x, y, t) in self.struct_parts(&a, &b, ty) {
                    let e = self.eq_values(&x, &y, t);
                    self.when(e, no);
                }
            }
            TyKind::Adt(..) => {
                let (ta, tb) = (
                    Operand::Copy(proj(&a, Proj::Field(0))),
                    Operand::Copy(proj(&b, Proj::Field(0))),
                );
                let same = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, ta, tb));
                self.when(same, no);
                let bb = b.clone();
                self.for_each_variant(&a, ty, |lw, v, parts| {
                    let view = lw.cx.view(ty, v);
                    let bv = proj(&bb, Proj::Cast(view));
                    for (k, (x, t)) in parts.into_iter().enumerate() {
                        let y = proj(&bv, Proj::Field(1 + k as u32));
                        let e = lw.eq_values(&x, &y, t);
                        lw.when(e, no);
                    }
                });
            }
            TyKind::Option(e) => self.eq_option(&a, &b, ty, e, no),
            TyKind::Array(e) => self.eq_array(&a, &b, e, no),
            TyKind::Shared(e) => {
                let bx = self.cx.shared_box(e);
                let inner = |p: &Place| proj(&proj(p, Proj::Deref(Ty::Agg(bx))), Proj::Field(1));
                let r = self.eq_values(&inner(&a), &inner(&b), e);
                self.when(r, no);
            }
            k => crate::lower::ice(format_args!("no equality glue for {k:?}")),
        }
        self.terminate(Terminator::Return(Operand::Const(
            Const::Bool(true),
            Ty::Bool,
        )));
        self.switch_to(no);
        self.terminate(Terminator::Return(Operand::Const(
            Const::Bool(false),
            Ty::Bool,
        )));
    }

    fn eq_option(&mut self, a: &Place, b: &Place, ty: TyId, e: TyId, no: vir::BlockId) {
        let sa = self.option_is_some(a, ty);
        let sb = self.option_is_some(b, ty);
        let same = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, sa.clone(), sb));
        self.when(same, no);
        let yes = self.new_block();
        self.when(sa, yes);
        let (pa, pb) = (self.some_payload(a, ty), self.some_payload(b, ty));
        let r = self.eq_values(&pa, &pb, e);
        self.when(r, no);
        self.goto(yes);
        self.switch_to(yes);
    }

    fn eq_array(&mut self, a: &Place, b: &Place, e: TyId, no: vir::BlockId) {
        let la = self.rvalue_temp(Ty::U64, Rvalue::Use(Operand::Copy(proj(a, Proj::Field(1)))));
        let lb = Operand::Copy(proj(b, Proj::Field(1)));
        let same = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, la.clone(), lb));
        self.when(same, no);
        let k = self.temp(Ty::U64);
        self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
        self.count_loop(k, la, |lw, k| {
            let x = lw.elem_place(a, k.clone(), e);
            let y = lw.elem_place(b, k, e);
            let r = lw.eq_values(&x, &y, e);
            lw.when(r, no);
        });
    }

    pub(super) fn hash_body(&mut self, p: vir::Local, ty: TyId) {
        let place = self.deref_param(p, ty);
        let h = self.temp(Ty::U64);
        self.assign(Place::local(h), Rvalue::Use(cint(0, Ty::U64)));
        let hv = Operand::Copy(Place::local(h));
        let add = |lw: &mut Self, x: Operand| {
            let n = lw.fx_combine(hv.clone(), x);
            lw.assign(Place::local(h), Rvalue::Use(n));
        };
        match self.cx.kind(ty) {
            TyKind::Adt(..) | TyKind::Tuple(_) if !self.is_enum(ty) => {
                for (x, _, t) in self.struct_parts(&place, &place, ty) {
                    let v = self.hash_value(&x, t);
                    add(self, v);
                }
            }
            TyKind::Adt(..) => {
                let tag = Operand::Copy(proj(&place, Proj::Field(0)));
                let t64 = self.cast_to(tag, Ty::U32, Ty::U64);
                add(self, t64);
                self.for_each_variant(&place, ty, |lw, _, parts| {
                    for (x, t) in parts {
                        let v = lw.hash_value(&x, t);
                        add(lw, v);
                    }
                });
            }
            TyKind::Option(e) => {
                let some = self.option_is_some(&place, ty);
                let s64 = self.cast_to(some.clone(), Ty::Bool, Ty::U64);
                add(self, s64);
                let done = self.new_block();
                self.when(some, done);
                let payload = self.some_payload(&place, ty);
                let v = self.hash_value(&payload, e);
                add(self, v);
                self.goto(done);
                self.switch_to(done);
            }
            TyKind::Array(e) => {
                let len = self.rvalue_temp(
                    Ty::U64,
                    Rvalue::Use(Operand::Copy(proj(&place, Proj::Field(1)))),
                );
                add(self, len.clone());
                let k = self.temp(Ty::U64);
                self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
                self.count_loop(k, len, |lw, k| {
                    let x = lw.elem_place(&place, k, e);
                    let v = lw.hash_value(&x, e);
                    add(lw, v);
                });
            }
            k => crate::lower::ice(format_args!("no hash glue for {k:?}")),
        }
        self.terminate(Terminator::Return(hv_final(h)));
    }
}

fn hv_final(h: vir::Local) -> Operand {
    Operand::Copy(Place::local(h))
}
