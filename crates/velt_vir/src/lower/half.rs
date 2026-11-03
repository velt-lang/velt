//! The storage floats `f16` and `bf16` (#2049). A value is its 16-bit pattern in a `u16`, so
//! VIR has no type of its own for them; what such a value means is this module's: the casts
//! to and from it are the conversion math operations (`velt_vir::math::MathOp::F16ToF32` &
//! co., one instruction or a branchless sequence in the backends), a literal is the bits of the
//! f64 value rounded once, and everything that reads a value as a number (printing, equality,
//! hashing) widens it to `f32` first. Sema widens the operands of every operator, so no VIR
//! arithmetic or comparison ever has one as an operand.

use velt_sema::hir::{self, FloatTy, TyId, TyKind};

use super::{cint, ice, Cx, FnLower};
use crate::math::{half, MathOp};
use crate::vir::{Const, Intrinsic, Operand, Rvalue, Ty};

impl Cx<'_> {
    /// The storage float `t` is, if it is one.
    pub(super) fn half(&self, t: TyId) -> Option<FloatTy> {
        match self.kind(t) {
            TyKind::Float(f) if f.is_storage() => Some(f),
            _ => None,
        }
    }
}

impl FnLower<'_, '_> {
    /// The storage float `t` is in this instantiation (its type parameters substituted), if
    /// it is one.
    pub(super) fn half_of(&mut self, t: TyId) -> Option<FloatTy> {
        let t = self.sub(t);
        self.cx.half(t)
    }

    /// The math operation `op` of `args`, its extern declared (vir.rs invariant 12).
    fn half_math(&mut self, op: MathOp, args: Vec<Operand>) -> Operand {
        let (params, ret) = op.signature();
        self.cx.extern_sym(op.symbol(), params.to_vec(), ret, false);
        self.rvalue_temp(ret, Rvalue::Intrinsic(Intrinsic::Math(op), args))
    }

    /// The storage float `v` (its bits) widened to `f32`, exactly.
    pub(super) fn widen_half(&mut self, v: Operand, h: FloatTy) -> Operand {
        let op = match h {
            FloatTy::F16 => MathOp::F16ToF32,
            _ => MathOp::Bf16ToF32,
        };
        self.half_math(op, vec![v])
    }

    /// The number `v` (of VIR type `from`) rounded once to the storage float `h`, as its bits.
    fn narrow_half(&mut self, v: Operand, from: Ty, h: FloatTy) -> Operand {
        let bf = h == FloatTy::BF16;
        let op = match from {
            Ty::F32 if bf => MathOp::F32ToBf16,
            Ty::F32 => MathOp::F32ToF16,
            Ty::F64 if bf => MathOp::F64ToBf16,
            Ty::F64 => MathOp::F64ToF16,
            // An integer beyond 2^53 would round twice through f64; a bf16 holds such values.
            Ty::I64 if bf => MathOp::I64ToBf16,
            Ty::U64 if bf => MathOp::U64ToBf16,
            // Every other integer is exact in f64 (and f16's range ends at 65504, so an f64
            // rounds the same as the integer would).
            t if t.is_int() || t == Ty::Bool => {
                let d = self.cast_to(v, t, Ty::F64);
                return self.narrow_half(d, Ty::F64, h);
            }
            t => ice(format_args!("a {t:?} narrowed to a storage float")),
        };
        self.half_math(op, vec![v])
    }

    /// `v` (a value of HIR type `from`) converted to HIR type `to` with `as` semantics, either
    /// of them possibly a storage float: through `f32` from one, rounded once into one.
    pub(super) fn num_cast(&mut self, v: Operand, from: TyId, to: TyId) -> Operand {
        let (fh, th) = (self.half_of(from), self.half_of(to));
        if fh.is_some() && fh == th {
            return v;
        }
        let (v, vfrom) = match fh {
            Some(h) => (self.widen_half(v, h), Ty::F32),
            None => (v, self.vty(from)),
        };
        match th {
            Some(h) => self.narrow_half(v, vfrom, h),
            None => {
                let vto = self.vty(to);
                self.cast_to(v, vfrom, vto)
            }
        }
    }

    /// The bits of table element `e` of storage float type `h`: a literal (negated or not)
    /// rounded once, a cast of a constant number (an integer into a `bf16` from the integer
    /// itself, never through f64) or of another storage float, or a module constant that is
    /// one. `None` for anything else, which is code.
    pub(super) fn table_half(&mut self, e: &hir::Expr, h: FloatTy, depth: u32) -> Option<u16> {
        let narrow = |f: f64| match h {
            FloatTy::F16 => half::f64_to_f16(f.to_bits()),
            _ => half::f64_to_bf16(f.to_bits()),
        };
        Some(match &e.kind {
            hir::ExprKind::Lit(hir::Lit::Float(f)) => narrow(*f),
            hir::ExprKind::Lit(hir::Lit::Int(n)) => narrow(*n as f64),
            hir::ExprKind::Unary {
                op: hir::UnOp::Neg,
                expr,
            } => self.table_half(expr, h, depth)? ^ 0x8000,
            hir::ExprKind::Cast(inner) => match self.half_of(inner.ty) {
                // Between the storage floats: through the exact f32.
                Some(from) => {
                    let b = self.table_half(inner, from, depth)?;
                    let w = match from {
                        FloatTy::F16 => half::f16_to_f32(b),
                        _ => half::bf16_to_f32(b),
                    };
                    narrow(f64::from(f32::from_bits(w)))
                }
                None => {
                    let from = self.vty(inner.ty);
                    match self.table_const(inner, from, depth)? {
                        Const::Float(f) => narrow(f),
                        Const::Int(n) if h == FloatTy::BF16 && from == Ty::U64 => {
                            half::u64_to_bf16(n as u64)
                        }
                        Const::Int(n) if h == FloatTy::BF16 => half::i64_to_bf16(n as i64),
                        Const::Int(n) => narrow(n as f64),
                        Const::Bool(b) => narrow(f64::from(u8::from(b))),
                        _ => return None,
                    }
                }
            },
            hir::ExprKind::Global(d) if depth < 16 => {
                let hir::Def::Global(g) = self.cx.hir.def(*d) else {
                    return None;
                };
                if self.half_of(g.ty) != Some(h) {
                    return None;
                }
                self.table_half(&g.init, h, depth + 1)?
            }
            _ => return None,
        })
    }

    /// The float `v` (of HIR type `t`: `f64`, `f32` or a storage float) as an `f64`, exactly,
    /// for code that reads it as a number (printing).
    pub(super) fn float_as_f64(&mut self, v: Operand, t: TyId) -> Operand {
        match self.half_of(t) {
            Some(h) => {
                let w = self.widen_half(v, h);
                self.cast_to(w, Ty::F32, Ty::F64)
            }
            None => {
                let from = self.cx.ty(t);
                self.cast_to(v, from, Ty::F64)
            }
        }
    }
}

/// The bits of the literal `f` (its f64 value, negated or not) rounded once to `h`.
pub(super) fn half_lit(f: f64, h: FloatTy) -> Operand {
    let bits = match h {
        FloatTy::F16 => half::f64_to_f16(f.to_bits()),
        _ => half::f64_to_bf16(f.to_bits()),
    };
    cint(i128::from(bits), Ty::U16)
}
