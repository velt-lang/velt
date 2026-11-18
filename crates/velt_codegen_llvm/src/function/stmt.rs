//! Statements: assignments of rvalues, aggregate construction, memory copies and fills
//! (`llvm.memcpy` / `llvm.memmove` / `llvm.memset`).

use velt_vir::vir::{AggId, Intrinsic, Operand, Place, Rvalue, Stmt, Ty};

use super::{Emitter, Loc, Val};
use crate::types::{abi_ret, abi_type, aggregate, global_name};
use crate::CodegenResult;

impl Emitter<'_> {
    pub(super) fn stmt(&mut self, s: &Stmt) -> CodegenResult<()> {
        match s {
            Stmt::Nop => Ok(()),
            Stmt::MemCopy { dst, src, size } => self.mem_copy(dst, src, *size),
            Stmt::MemCopyDyn {
                dst,
                src,
                len,
                overlapping,
            } => {
                let kind = if *overlapping { "memmove" } else { "memcpy" };
                let (dst, _) = self.scalar(dst)?;
                let (src, _) = self.scalar(src)?;
                let (len, _) = self.scalar(len)?;
                self.mem_intrinsic(kind, (&dst, 1), (&src, 1), &len);
                Ok(())
            }
            Stmt::MemSet { dst, byte, len } => self.mem_set(dst, byte, len),
            Stmt::Assign(place, rvalue) => self.assign(place, rvalue),
        }
    }

    /// VIR guarantees the ranges do not overlap; their alignment is unknown.
    fn mem_copy(&mut self, dst: &Operand, src: &Operand, size: u64) -> CodegenResult<()> {
        let (dst, _) = self.scalar(dst)?;
        let (src, _) = self.scalar(src)?;
        if size > 0 {
            self.mem_intrinsic("memcpy", (&dst, 1), (&src, 1), &size.to_string());
        }
        Ok(())
    }

    fn mem_set(&mut self, dst: &Operand, byte: &Operand, len: &Operand) -> CodegenResult<()> {
        let (dst, _) = self.scalar(dst)?;
        let (byte, _) = self.scalar(byte)?;
        let (len, _) = self.scalar(len)?;
        let name = "@llvm.memset.p0.i64";
        self.intrinsics
            .need(format!("declare void {name}(ptr, i8, i64, i1)"));
        self.line(format!(
            "call void {name}(ptr align 1 {dst}, i8 {byte}, i64 {len}, i1 false)"
        ));
        Ok(())
    }

    fn assign(&mut self, place: &Place, rvalue: &Rvalue) -> CodegenResult<()> {
        let val = match rvalue {
            Rvalue::Use(op) => self.operand(op)?.0,
            Rvalue::Unary(op, a) => Val::Scalar(self.unary(*op, a)?),
            Rvalue::Binary(op, a, b) => Val::Scalar(self.binary(*op, a, b)?),
            Rvalue::Cast(a, to) => {
                let (v, from) = self.scalar(a)?;
                Val::Scalar(self.cast(&v, from, *to)?)
            }
            Rvalue::AddrOf(p) => match self.place(p)? {
                loc @ Loc::Mem { .. } => Val::Scalar(self.address(&loc)?.0),
                Loc::Unit => bail!("cannot take the address of a unit place"),
            },
            Rvalue::Aggregate(id, ops) => return self.aggregate(place, *id, ops),
            Rvalue::Intrinsic(Intrinsic::Math(op), ops) => {
                let mut args = Vec::with_capacity(ops.len());
                for o in ops {
                    let (v, ty) = self.scalar(o)?;
                    args.push(format!("{} {v}", abi_type(ty)?));
                }
                // The runtime function for `pow` (declared: the verifier checks).
                let target = (self.math_target(*op)).unwrap_or_else(|| global_name(op.symbol()));
                let ret = abi_ret(op.signature().1)?;
                Val::Scalar(self.inst(format!("call {ret} {target}({})", args.join(", "))))
            }
        };
        let loc = self.place(place)?;
        self.write(loc, val)
    }

    /// Store each field at its offset. Operands are evaluated before the destination is
    /// written (as in the Cranelift backend), so scalar fields may read the old destination.
    fn aggregate(&mut self, place: &Place, id: AggId, ops: &[Operand]) -> CodegenResult<()> {
        let layout = aggregate(self.program, id)?;
        if ops.len() != layout.fields.len() {
            bail!(
                "aggregate `{}` has {} fields but {} operands were given",
                layout.name,
                layout.fields.len(),
                ops.len()
            );
        }
        let fields = layout.fields.clone();
        let mut vals = Vec::with_capacity(ops.len());
        for op in ops {
            vals.push(self.operand(op)?.0);
        }
        let Loc::Mem {
            base,
            offset,
            base_align,
            root,
            ..
        } = self.place(place)?
        else {
            bail!("aggregate assigned to a non-memory place")
        };
        // Two 128-bit vectors and nothing else is a 256-bit vector (`f32x8`,
        // `velt_vir::simd::wide`): one store of all 32 bytes, so that LLVM's scalar replacement
        // keeps the slot one `ymm`-sized value. A store per half would have it split the slot,
        // and every value copied to or from it, into two.
        if let ([(Ty::V128, 0), (Ty::V128, 16)], 32, [Val::Scalar(lo), Val::Scalar(hi)]) =
            (&fields[..], layout.size, &vals[..])
        {
            let whole = Loc::Mem {
                base,
                offset,
                base_align,
                ty: Ty::Agg(id),
                root,
                tbaa: None,
            };
            let (ptr, align) = self.address(&whole)?;
            let picks: Vec<String> = (0..32).map(|i| format!("i32 {i}")).collect();
            let both = self.inst(format!(
                "shufflevector <16 x i8> {lo}, <16 x i8> {hi}, <32 x i32> <{}>",
                picks.join(", ")
            ));
            self.line(format!("store <32 x i8> {both}, ptr {ptr}, align {align}"));
            return Ok(());
        }
        for (val, (field_ty, field_offset)) in vals.into_iter().zip(fields) {
            let loc = Loc::Mem {
                base: base.clone(),
                offset: offset + u64::from(field_offset),
                base_align,
                ty: field_ty,
                root,
                tbaa: None,
            };
            self.write(loc, val)?;
        }
        Ok(())
    }
}
