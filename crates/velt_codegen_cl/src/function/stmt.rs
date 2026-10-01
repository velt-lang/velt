//! Statements: assignments of rvalues, aggregate construction, memory copies and fills
//! (libc `memcpy`/`memmove`/`memset` for runtime lengths), unary ops.

use cranelift_codegen::ir::{self, types, InstBuilder};
use cranelift_module::Module;
use velt_vir::vir::{AggId, Operand, Place, Rvalue, Stmt, Ty, UnOp};

use super::place::add_offset;
use super::{Loc, Translator, Val};
use crate::abi::aggregate;
use crate::CodegenResult;

/// `MemCopy`s up to this size are inlined as loads/stores.
const INLINE_COPY_LIMIT: u64 = 64;

impl<M: Module> Translator<'_, '_, M> {
    pub(super) fn stmt(&mut self, s: &Stmt) -> CodegenResult<()> {
        match s {
            Stmt::Nop => Ok(()),
            Stmt::MemCopy { dst, src, size } => self.mem_copy(dst, src, *size),
            Stmt::MemCopyDyn {
                dst,
                src,
                len,
                overlapping,
            } => self.mem_copy_dyn(dst, src, len, *overlapping),
            Stmt::MemSet { dst, byte, len } => self.mem_set(dst, byte, len),
            Stmt::Assign(place, rvalue) => self.assign(place, rvalue),
        }
    }

    fn mem_copy(&mut self, dst: &Operand, src: &Operand, size: u64) -> CodegenResult<()> {
        let (dst, _) = self.scalar(dst)?;
        let (src, _) = self.scalar(src)?;
        if size <= INLINE_COPY_LIMIT {
            self.copy(dst, src, size, 1, true);
        } else {
            let n = self.builder.ins().iconst(types::I64, size as i64);
            self.builder.call_memcpy(self.frontend_config, dst, src, n);
        }
        Ok(())
    }

    fn mem_copy_dyn(
        &mut self,
        dst: &Operand,
        src: &Operand,
        len: &Operand,
        overlapping: bool,
    ) -> CodegenResult<()> {
        let (dst, _) = self.scalar(dst)?;
        let (src, _) = self.scalar(src)?;
        let (len, _) = self.scalar(len)?;
        if overlapping {
            self.builder
                .call_memmove(self.frontend_config, dst, src, len);
        } else {
            self.builder
                .call_memcpy(self.frontend_config, dst, src, len);
        }
        Ok(())
    }

    fn mem_set(&mut self, dst: &Operand, byte: &Operand, len: &Operand) -> CodegenResult<()> {
        let (dst, _) = self.scalar(dst)?;
        let (byte, _) = self.scalar(byte)?;
        let (len, _) = self.scalar(len)?;
        self.builder
            .call_memset(self.frontend_config, dst, byte, len);
        Ok(())
    }

    fn assign(&mut self, place: &Place, rvalue: &Rvalue) -> CodegenResult<()> {
        let val = match rvalue {
            Rvalue::Use(op) => self.operand(op)?.0,
            Rvalue::Unary(op, a) => Val::Scalar(self.unary(*op, a)?),
            Rvalue::Binary(op, a, b) => Val::Scalar(self.binary(*op, a, b)?),
            Rvalue::Cast(a, to) => {
                let (v, from) = self.scalar(a)?;
                Val::Scalar(self.cast(v, from, *to)?)
            }
            Rvalue::AddrOf(p) => match self.place(p)? {
                Loc::Mem(base, offset, _) => Val::Scalar(self.addr(base, offset)),
                _ => bail!("cannot take the address of a register or unit place"),
            },
            Rvalue::Aggregate(id, ops) => return self.aggregate(place, *id, ops),
        };
        let loc = self.place(place)?;
        self.write(loc, val)
    }

    /// Store each field at its offset. Operands are evaluated before the destination is
    /// written, so fields may read the old value of the destination.
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
        let Loc::Mem(base, offset, _) = self.place(place)? else {
            bail!("aggregate assigned to a non-memory place")
        };
        for (val, (field_ty, field_offset)) in vals.into_iter().zip(fields) {
            let loc = Loc::Mem(base, add_offset(offset, field_offset)?, field_ty);
            self.write(loc, val)?;
        }
        Ok(())
    }

    fn unary(&mut self, op: UnOp, a: &Operand) -> CodegenResult<ir::Value> {
        let (v, ty) = self.scalar(a)?;
        Ok(match op {
            UnOp::Neg if ty.is_float() => self.builder.ins().fneg(v),
            UnOp::Neg if ty.is_int() => self.builder.ins().ineg(v),
            UnOp::Not if ty == Ty::Bool => self.builder.ins().bxor_imm(v, 1),
            UnOp::BitNot if ty.is_int() => self.builder.ins().bnot(v),
            _ => bail!("unary {op:?} is not defined on {ty:?}"),
        })
    }
}
