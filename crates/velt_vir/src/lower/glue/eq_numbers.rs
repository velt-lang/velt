//! Equality and hash of a union with several number members (`f64 | u8`, #1890). Every number
//! is a number in TypeScript, so two such values are equal when their numbers are, whichever
//! members hold them: `1.0` equals `1u8`, as both print `1`. The comparison is exact (an `i64`
//! above 2^53 equals no `f64` it only rounds to) and keeps the glue's mode: `toEqual` tells `-0`
//! from `0` (SameValue), keys do not (SameValueZero). The hash agrees: a number member hashes
//! its value as a key `f64`, without the member's tag.

use velt_sema::hir::{TyId, TyKind};

use crate::lower::{cint, FnLower};
use crate::vir::{BinOp, Const, Operand, Place, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// The union `ty` has two or more members that are numbers (integer or float types).
    pub(super) fn several_numbers(&mut self, ty: TyId) -> bool {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return false;
        };
        let n = self.cx.enum_def(d).variants.len() as u32;
        (0..n)
            .filter(|&v| self.number_member(ty, v).is_some())
            .count()
            >= 2
    }

    /// The number type of member `v` of union `ty`, when that member is a number.
    fn number_member(&mut self, ty: TyId, v: u32) -> Option<TyId> {
        let parts: Vec<TyId> = self
            .cx
            .variant_tys(ty, v)
            .into_iter()
            .filter(|&t| !self.cx.is_unit(t))
            .collect();
        match parts[..] {
            [t] if matches!(self.cx.kind(t), TyKind::Int(_) | TyKind::Float(_)) => Some(t),
            _ => None,
        }
    }

    /// Whether two values of union `ty` that hold different members (their tags differ) are
    /// equal: both members are numbers with equal values (Bool operand).
    pub(super) fn numbers_eq(&mut self, a: &Place, b: &Place, ty: TyId) -> Operand {
        let r = self.temp(Ty::Bool);
        self.assign(
            Place::local(r),
            Rvalue::Use(Operand::Const(Const::Bool(false), Ty::Bool)),
        );
        let b = b.clone();
        self.for_each_variant(a, ty, |lw, v, parts| {
            let Some(tx) = lw.number_member(ty, v) else {
                return;
            };
            let x = parts[0].0.clone();
            lw.for_each_variant(&b, ty, |lw, w, parts| {
                let Some(ty_) = lw.number_member(ty, w).filter(|_| w != v) else {
                    return;
                };
                let e = lw.number_eq(&x, tx, &parts[0].0, ty_);
                lw.assign(Place::local(r), Rvalue::Use(e));
            });
        });
        Operand::Copy(Place::local(r))
    }

    /// Two numbers of the number types `tx` and `ty`, compared exactly.
    fn number_eq(&mut self, x: &Place, tx: TyId, y: &Place, ty: TyId) -> Operand {
        let (vx, vy) = (self.cx.ty(tx), self.cx.ty(ty));
        let (x, y) = (Operand::Copy(x.clone()), Operand::Copy(y.clone()));
        match (vx.is_float(), vy.is_float()) {
            (true, true) => {
                let (x, y) = (self.cast_to(x, vx, Ty::F64), self.cast_to(y, vy, Ty::F64));
                self.floats_eq(x, y)
            }
            (false, false) => self.ints_eq(x, vx, y, vy),
            (false, true) => self.int_float_eq(x, vx, y, vy),
            (true, false) => self.int_float_eq(y, vy, x, vx),
        }
    }

    /// Two `f64`s by the glue's mode: SameValue for `toEqual` and value comparison, SameValueZero
    /// for keys, `==` otherwise.
    fn floats_eq(&mut self, x: Operand, y: Operand) -> Operand {
        if self.deep_mode || self.value_mode {
            self.same_value(x, y, Ty::F64)
        } else if self.key_mode {
            self.same_value_zero(x, y)
        } else {
            self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, x, y))
        }
    }

    /// Two integers of different types: a signed one equals an unsigned one only when it is not
    /// negative.
    fn ints_eq(&mut self, x: Operand, vx: Ty, y: Operand, vy: Ty) -> Operand {
        if vx.is_signed() == vy.is_signed() {
            let w = if vx.is_signed() { Ty::I64 } else { Ty::U64 };
            let (x, y) = (self.cast_to(x, vx, w), self.cast_to(y, vy, w));
            return self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, x, y));
        }
        let ((s, vs), (u, vu)) = if vx.is_signed() {
            ((x, vx), (y, vy))
        } else {
            ((y, vy), (x, vx))
        };
        let s = self.cast_to(s, vs, Ty::I64);
        let u = self.cast_to(u, vu, Ty::U64);
        let sign = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ge, s.clone(), cint(0, Ty::I64)),
        );
        let su = self.cast_to(s, Ty::I64, Ty::U64);
        let same = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, su, u));
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, sign, same))
    }

    /// An integer and a float: the float is the integer's value, converted (which rounds) and
    /// back (which truncates and saturates), below the integer type's 2^63 or 2^64 bound.
    fn int_float_eq(&mut self, i: Operand, vi: Ty, f: Operand, vf: Ty) -> Operand {
        let f = self.cast_to(f, vf, Ty::F64);
        let fi = self.cast_to(i.clone(), vi, Ty::F64);
        let near = self.floats_eq(f.clone(), fi);
        let (w, bound) = if vi.is_signed() {
            (Ty::I64, 9_223_372_036_854_775_808.0)
        } else {
            (Ty::U64, 18_446_744_073_709_551_616.0)
        };
        let below = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(
                BinOp::Lt,
                f.clone(),
                Operand::Const(Const::Float(bound), Ty::F64),
            ),
        );
        let back = self.cast_to(f, Ty::F64, w);
        let iw = self.cast_to(i, vi, w);
        let exact = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, back, iw));
        let both = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, near, below));
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, both, exact))
    }

    /// The hash part of member `v` of union `ty` when it is a number: its value as a key `f64`
    /// (`-0` as `0`, every `NaN` alike), so equal numbers in different members hash alike.
    pub(super) fn number_hash(&mut self, ty: TyId, v: u32, x: &Place) -> Option<Operand> {
        let t = self.number_member(ty, v)?;
        let vt = self.cx.ty(t);
        let f = self.cast_to(Operand::Copy(x.clone()), vt, Ty::F64);
        Some(self.key_float_bits(f, Ty::F64))
    }
}
