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
        match self.kind(self.ph_rep(t)) {
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

    /// `inner as to`: through the conversion operations when either is an `f16`/`bf16`
    /// (`num_cast`), after the fast path below.
    pub(super) fn cast_expr(&mut self, inner: &hir::Expr, to: TyId) -> Operand {
        if let Some(v) = self.half_fast(inner, to) {
            return v;
        }
        let v = self.expr(inner);
        if self.half_of(inner.ty).is_some() || self.half_of(to).is_some() {
            return self.num_cast(v, inner.ty, to);
        }
        let (from, vto) = (self.vty(inner.ty), self.vty(to));
        self.cast_to(v, from, vto)
    }

    /// One `+ - * /` of two values of the storage float `to`, or one and a literal exact in
    /// it, rounded into `to` (#2191, owner decision): computed in f32 and narrowed f32 -> `to`,
    /// one instruction where the target has it, instead of in f64 and narrowed through f32
    /// rounded to odd. Exact by theorem: rounding twice through a format of p' >= 2p + 2 bits
    /// is innocuous for one basic operation on p-bit operands (Figueroa 1995), and f32 has 24
    /// bits against f16's 11 and bf16's 8. Checked exhaustively over every pair of 16-bit
    /// patterns of both formats (`tests/half_fast.rs`). `None`
    /// for anything else: another operator, a `number` operand, a literal the format cannot
    /// hold (`x * 0.999`), or a chain of operations.
    fn half_fast(&mut self, e: &hir::Expr, to: TyId) -> Option<Operand> {
        let h = self.half_of(to)?;
        let hir::ExprKind::Binary { op, lhs, rhs } = &e.kind else {
            return None;
        };
        let arith = matches!(
            op,
            hir::BinOp::Add | hir::BinOp::Sub | hir::BinOp::Mul | hir::BinOp::Div
        );
        if !arith || self.vty(e.ty) != Ty::F64 {
            return None;
        }
        let (a, b) = (self.fast_operand(lhs, h)?, self.fast_operand(rhs, h)?);
        let x = self.fast_value(lhs, a, h);
        let y = self.fast_value(rhs, b, h);
        let r = self.rvalue_temp(Ty::F32, Rvalue::Binary(super::ops::arith_op(*op), x, y));
        Some(self.narrow_half(r, Ty::F32, h))
    }

    /// What `half_fast` takes as an operand: a value of the storage float `h` widened to a
    /// number (`None` inside), or a literal exact in `h` (its value).
    fn fast_operand(&mut self, e: &hir::Expr, h: FloatTy) -> Option<Option<f64>> {
        if let hir::ExprKind::Cast(inner) = &e.kind {
            return (self.half_of(inner.ty) == Some(h)).then_some(None);
        }
        let v = literal_value(e)?;
        let bits = match h {
            FloatTy::F16 => half::f16_to_f32(half::f64_to_f16(v.to_bits())),
            _ => half::bf16_to_f32(half::f64_to_bf16(v.to_bits())),
        };
        let exact = f64::from(f32::from_bits(bits)).to_bits() == v.to_bits();
        exact.then_some(Some(v))
    }

    /// The f32 operand `fast_operand` described: the value widened, or the literal.
    fn fast_value(&mut self, e: &hir::Expr, lit: Option<f64>, h: FloatTy) -> Operand {
        match (lit, &e.kind) {
            (Some(v), _) => Operand::Const(Const::Float(v), Ty::F32),
            (None, hir::ExprKind::Cast(inner)) => {
                let v = self.expr(inner);
                self.widen_half(v, h)
            }
            _ => ice("a fast storage-float operand that is neither"),
        }
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

impl FnLower<'_, '_> {
    /// Records that `v`, the value of an expression of HIR type `t`, is a storage float's bits
    /// when it is a local (debug builds of the compiler).
    pub(super) fn note_half(&mut self, t: TyId, v: &Operand) {
        if let Operand::Copy(p) = v {
            if p.proj.is_empty() && self.half_of(t).is_some() {
                self.half_locals.insert(p.local.0);
            }
        }
    }

    /// The check of #2049's "bits are never compared" (debug builds of the compiler, so every
    /// test and golden runs it): no arithmetic, comparison, unary operator, numeric cast or
    /// switch reads a local recorded by [`note_half`](Self::note_half). Its reads go through
    /// the conversion operations (`widen_half`), loads, stores, calls and aggregates.
    pub(super) fn check_half_bits(&self, symbol: &str) {
        use crate::vir::{Stmt, Terminator};
        if self.half_locals.is_empty() {
            return;
        }
        let bits = |o: &Operand| matches!(o, Operand::Copy(p) if p.proj.is_empty() && self.half_locals.contains(&p.local.0));
        for (stmts, term) in &self.blocks {
            for st in stmts {
                let Stmt::Assign(_, rv) = st else {
                    continue;
                };
                let bad = match rv {
                    Rvalue::Binary(_, a, b) => bits(a) || bits(b),
                    Rvalue::Unary(_, a) | Rvalue::Cast(a, _) => bits(a),
                    _ => false,
                };
                if bad {
                    ice(format_args!(
                        "{symbol}: an operator reads an f16/bf16's bits as a number: {rv:?}"
                    ));
                }
            }
            if let Some(Terminator::Switch { value, .. }) = term {
                if bits(value) {
                    ice(format_args!("{symbol}: a switch on an f16/bf16's bits"));
                }
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

/// The value of a number literal, possibly negated.
fn literal_value(e: &hir::Expr) -> Option<f64> {
    match &e.kind {
        hir::ExprKind::Lit(hir::Lit::Float(f)) => Some(*f),
        hir::ExprKind::Lit(hir::Lit::Int(n)) => Some(*n as f64),
        hir::ExprKind::Unary {
            op: hir::UnOp::Neg,
            expr,
        } => literal_value(expr).map(|v| -v),
        _ => None,
    }
}
