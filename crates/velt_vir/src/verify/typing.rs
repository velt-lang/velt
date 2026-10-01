//! Type checking of places, operands, rvalues, statements and terminators (ids in range, operand
//! type agreement, call signatures, noreturn calls followed by `Unreachable`).

use super::FnCheck;
use crate::lower::successors;
use crate::vir::*;

type R<T> = Result<T, String>;

impl FnCheck<'_> {
    pub(super) fn place_ty(&self, pl: &Place) -> R<Ty> {
        let local = self.f.locals.get(pl.local.0 as usize);
        let mut t = local
            .ok_or_else(|| format!("unknown local _{}", pl.local.0))?
            .ty;
        for pr in &pl.proj {
            t = match (pr, t) {
                (Proj::Field(n), Ty::Agg(a)) => {
                    let fields = &self.p.aggs[a.0 as usize].fields;
                    fields
                        .get(*n as usize)
                        .ok_or_else(|| {
                            format!("invalid projection: field {n} out of range for agg#{}", a.0)
                        })?
                        .0
                }
                (Proj::Deref(to), Ty::Ptr) => self.check_ty(*to)?,
                (Proj::Cast(a), Ty::Agg(_)) => self.check_ty(Ty::Agg(*a))?,
                (pr, t) => return Err(format!("invalid projection {pr:?} on {t:?}")),
            };
        }
        Ok(t)
    }

    fn check_ty(&self, t: Ty) -> R<Ty> {
        match t {
            Ty::Agg(a) if a.0 as usize >= self.p.aggs.len() => Err(format!("unknown agg#{}", a.0)),
            t => Ok(t),
        }
    }

    fn operand_ty(&self, o: &Operand) -> R<Ty> {
        match o {
            Operand::Copy(pl) => self.place_ty(pl),
            Operand::Const(c, t) => self.const_ty(c, *t),
        }
    }

    fn const_ty(&self, c: &Const, t: Ty) -> R<Ty> {
        self.check_ty(t)?;
        let in_range = |id: u32, len: usize, what: &str| {
            if (id as usize) < len {
                Ok(t == Ty::Ptr)
            } else {
                Err(format!("unknown {what} #{id}"))
            }
        };
        let ok = match c {
            Const::Int(_) => t.is_int() || t == Ty::Ptr || t == Ty::Bool,
            Const::Float(_) => t.is_float(),
            Const::Bool(_) => t == Ty::Bool,
            Const::Unit => t == Ty::Unit,
            Const::Static(s) => in_range(s.0, self.p.statics.len(), "static")?,
            Const::Func(x) => in_range(x.0, self.p.funcs.len(), "function")?,
            Const::Extern(x) => in_range(x.0, self.p.externs.len(), "extern")?,
        };
        if ok {
            Ok(t)
        } else {
            Err(format!("constant {c:?} does not fit type {t:?}"))
        }
    }

    fn scalar_operand(&self, o: &Operand) -> R<Ty> {
        let t = self.operand_ty(o)?;
        if !t.is_scalar() {
            return Err(format!("expected a scalar operand, found {t:?}"));
        }
        Ok(t)
    }

    fn rvalue_ty(&self, rv: &Rvalue) -> R<Ty> {
        match rv {
            Rvalue::Use(o) => self.operand_ty(o),
            Rvalue::Unary(op, o) => {
                let t = self.scalar_operand(o)?;
                let ok = match op {
                    UnOp::Neg => t.is_int() || t.is_float(),
                    UnOp::Not => t == Ty::Bool,
                    UnOp::BitNot => t.is_int(),
                };
                ok.then_some(t)
                    .ok_or_else(|| format!("{op:?} not applicable to {t:?}"))
            }
            Rvalue::Binary(op, a, b) => {
                self.binary_ty(*op, self.scalar_operand(a)?, self.scalar_operand(b)?)
            }
            Rvalue::Cast(o, to) => {
                let from = self.scalar_operand(o)?;
                let num = |t: Ty| t.is_int() || t.is_float();
                // int → Bool is `!= 0` (not produced by lowering, but backends support it).
                let ok = (num(from) && num(*to))
                    || (from == Ty::Bool && to.is_int())
                    || (from.is_int() && *to == Ty::Bool)
                    || (from.is_int() && *to == Ty::Ptr)
                    || (from == Ty::Ptr && to.is_int());
                ok.then_some(*to)
                    .ok_or_else(|| format!("invalid cast {from:?} as {to:?}"))
            }
            Rvalue::AddrOf(pl) => self.place_ty(pl).map(|_| Ty::Ptr),
            Rvalue::Aggregate(a, ops) => self.aggregate_ty(*a, ops),
        }
    }

    fn binary_ty(&self, op: BinOp, ta: Ty, tb: Ty) -> R<Ty> {
        use BinOp::*;
        let ok = match op {
            PtrAdd => ta == Ty::Ptr && matches!(tb, Ty::I64 | Ty::U64),
            Shl | Shr | UShr => ta.is_int() && tb.is_int(),
            _ if ta != tb => return Err(format!("{op:?} operand types differ: {ta:?} vs {tb:?}")),
            Add | Sub | Mul | Div | Rem => ta.is_int() || ta.is_float(),
            BitAnd | BitOr | BitXor => ta.is_int() || ta == Ty::Bool,
            Eq | Ne | Lt | Le | Gt | Ge => return Ok(Ty::Bool),
        };
        ok.then_some(ta)
            .ok_or_else(|| format!("{op:?} not applicable to {ta:?}, {tb:?}"))
    }

    fn aggregate_ty(&self, a: AggId, ops: &[Operand]) -> R<Ty> {
        self.check_ty(Ty::Agg(a))?;
        let fields = &self.p.aggs[a.0 as usize].fields;
        if fields.len() != ops.len() {
            return Err(format!(
                "aggregate agg#{} expects {} fields, got {}",
                a.0,
                fields.len(),
                ops.len()
            ));
        }
        for (i, (o, (ft, _))) in ops.iter().zip(fields).enumerate() {
            let t = self.operand_ty(o)?;
            if t != *ft {
                return Err(format!(
                    "aggregate agg#{} field {i}: expected {ft:?}, found {t:?}",
                    a.0
                ));
            }
        }
        Ok(Ty::Agg(a))
    }

    fn expect_operand(&self, o: &Operand, want: Ty, what: &str) -> R<()> {
        match self.operand_ty(o)? {
            t if t == want => Ok(()),
            t => Err(format!("{what} has type {t:?}, expected {want:?}")),
        }
    }

    pub(super) fn check_stmt(&self, s: &Stmt) -> R<()> {
        match s {
            Stmt::Assign(pl, rv) => {
                let (pt, rt) = (self.place_ty(pl)?, self.rvalue_ty(rv)?);
                if pt != rt {
                    return Err(format!(
                        "type mismatch: assignment of {rt:?} to place of type {pt:?}"
                    ));
                }
            }
            Stmt::MemCopy { dst, src, .. } => {
                if self.operand_ty(dst)? != Ty::Ptr || self.operand_ty(src)? != Ty::Ptr {
                    return Err("MemCopy operands must be Ptr".into());
                }
            }
            Stmt::MemCopyDyn { dst, src, len, .. } => {
                self.expect_operand(dst, Ty::Ptr, "memcopy destination")?;
                self.expect_operand(src, Ty::Ptr, "memcopy source")?;
                self.expect_operand(len, Ty::U64, "memcopy length")?;
            }
            Stmt::MemSet { dst, byte, len } => {
                self.expect_operand(dst, Ty::Ptr, "memset destination")?;
                self.expect_operand(byte, Ty::U8, "memset byte")?;
                self.expect_operand(len, Ty::U64, "memset length")?;
            }
            Stmt::Nop => {}
        }
        Ok(())
    }

    pub(super) fn check_term(&self, t: &Terminator) -> R<()> {
        for s in successors(t) {
            if s.0 as usize >= self.f.blocks.len() {
                return Err(format!("unknown block bb{}", s.0));
            }
        }
        let expect = |got: Ty, want: Ty, what: &str| {
            if got == want {
                Ok(())
            } else {
                Err(format!("{what} has type {got:?}, expected {want:?}"))
            }
        };
        match t {
            Terminator::Goto(_) | Terminator::Unreachable => Ok(()),
            Terminator::Branch { cond, .. } => {
                expect(self.operand_ty(cond)?, Ty::Bool, "branch condition")
            }
            Terminator::Switch { value, .. } => match self.operand_ty(value)? {
                vt if vt.is_int() => Ok(()),
                vt => Err(format!("switch on non-integer {vt:?}")),
            },
            Terminator::Return(o) => expect(self.operand_ty(o)?, self.f.ret, "return value"),
            Terminator::Call {
                callee,
                args,
                dest,
                next,
            } => self.check_call(callee, args, dest.as_ref(), *next),
        }
    }

    /// (params, ret, noreturn) of a callee.
    fn callee_sig<'c>(&'c self, callee: &'c Callee) -> R<(&'c [Ty], Ty, bool)> {
        match callee {
            Callee::Func(fi) => {
                let f = self
                    .p
                    .funcs
                    .get(fi.0 as usize)
                    .ok_or_else(|| format!("unknown function fn#{}", fi.0))?;
                Ok((&f.params, f.ret, false))
            }
            Callee::Extern(e) => {
                let e = self
                    .p
                    .externs
                    .get(e.0 as usize)
                    .ok_or_else(|| format!("unknown extern extern#{}", e.0))?;
                Ok((&e.params, e.ret, e.noreturn))
            }
            Callee::Ptr {
                target,
                params,
                ret,
            } => {
                if self.operand_ty(target)? != Ty::Ptr {
                    return Err("indirect call target must be Ptr".into());
                }
                if params.iter().any(|t| !t.is_scalar()) || matches!(ret, Ty::Agg(_)) {
                    return Err("indirect call signature is not scalar-only".into());
                }
                Ok((params, *ret, false))
            }
        }
    }

    fn check_call(
        &self,
        callee: &Callee,
        args: &[Operand],
        dest: Option<&Place>,
        next: BlockId,
    ) -> R<()> {
        let (params, ret, noreturn) = self.callee_sig(callee)?;
        if params.len() != args.len() {
            return Err(format!(
                "call passes {} args, callee takes {}",
                args.len(),
                params.len()
            ));
        }
        for (i, (a, pt)) in args.iter().zip(params).enumerate() {
            let at = self.operand_ty(a)?;
            if at != *pt {
                return Err(format!("call arg {i}: expected {pt:?}, found {at:?}"));
            }
        }
        match (dest, ret) {
            (None, Ty::Unit) => {}
            // `(Some(dest), Unit)`: a Unit-typed dummy destination is permitted (vir.rs `Ty::Unit`).
            (None, _) => {
                return Err("call result discarded (dest is None for non-Unit callee)".into())
            }
            (Some(d), r) => {
                let dt = self.place_ty(d)?;
                if dt != r {
                    return Err(format!(
                        "call destination has type {dt:?}, callee returns {r:?}"
                    ));
                }
            }
        }
        let nb = &self.f.blocks[next.0 as usize];
        if noreturn && (!nb.stmts.is_empty() || nb.term != Terminator::Unreachable) {
            return Err(format!(
                "noreturn call must continue to an `Unreachable` block (bb{})",
                next.0
            ));
        }
        Ok(())
    }
}
