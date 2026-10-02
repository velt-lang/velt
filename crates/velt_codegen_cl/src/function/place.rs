//! Places: resolving locals + projections to registers or memory, and reading/writing them.

use cranelift_codegen::ir::{self, types, InstBuilder, MemFlags};
use cranelift_module::Module;
use velt_vir::vir::{AggId, Place, Proj, Ty};

use super::{Loc, Storage, Translator, Val};
use crate::abi::{aggregate, cl_type, scalar_type, size_align};
use crate::CodegenResult;

/// Memory accesses may go through arbitrary (lowering-checked) pointers, so no `notrap`.
pub(super) fn mem_flags() -> MemFlags {
    MemFlags::new()
}

/// Add a field offset, rejecting offsets that do not fit Cranelift's 32-bit immediates.
pub(super) fn add_offset(offset: i32, field_offset: u32) -> CodegenResult<i32> {
    match i32::try_from(i64::from(offset) + i64::from(field_offset)) {
        Ok(o) => Ok(o),
        Err(_) => bail!("field offset too large"),
    }
}

impl<M: Module> Translator<'_, '_, M> {
    pub(super) fn place(&mut self, p: &Place) -> CodegenResult<Loc> {
        let index = p.local.0 as usize;
        let Some(decl) = self.function.locals.get(index) else {
            bail!("unknown local _{index}")
        };
        let mut loc = match self.storage[index] {
            Storage::Var(v) => Loc::Var(v, decl.ty),
            Storage::Value(local) => Loc::Value(local, decl.ty),
            Storage::Slot(slot) => {
                let addr = self.builder.ins().stack_addr(types::I64, slot, 0);
                Loc::Mem(addr, 0, decl.ty)
            }
            Storage::Unit => Loc::Unit,
        };
        for proj in &p.proj {
            loc = self.project(loc, proj)?;
        }
        Ok(loc)
    }

    fn project(&mut self, loc: Loc, proj: &Proj) -> CodegenResult<Loc> {
        Ok(match (proj, loc) {
            (Proj::Field(n), Loc::Mem(base, offset, Ty::Agg(id))) => {
                let (field_ty, field_offset) = self.field(id, *n)?;
                Loc::Mem(base, add_offset(offset, field_offset)?, field_ty)
            }
            (Proj::Deref(pointee), l) if l.ty() == Ty::Ptr => {
                let Val::Scalar(ptr) = self.read(l)? else {
                    unreachable!("ICE: a Ptr place always reads as a scalar")
                };
                Loc::Mem(ptr, 0, *pointee)
            }
            (Proj::Cast(id), Loc::Mem(base, offset, _)) => {
                aggregate(self.program, *id)?;
                Loc::Mem(base, offset, Ty::Agg(*id))
            }
            (proj, l) => bail!(
                "invalid projection {proj:?} on a place of type {:?}",
                l.ty()
            ),
        })
    }

    fn field(&self, id: AggId, n: u32) -> CodegenResult<(Ty, u32)> {
        let layout = aggregate(self.program, id)?;
        match layout.fields.get(n as usize) {
            Some(&f) => Ok(f),
            None => bail!("aggregate `{}` has no field {n}", layout.name),
        }
    }

    pub(super) fn addr(&mut self, base: ir::Value, offset: i32) -> ir::Value {
        if offset == 0 {
            base
        } else {
            self.builder.ins().iadd_imm(base, i64::from(offset))
        }
    }

    pub(super) fn read(&mut self, loc: Loc) -> CodegenResult<Val> {
        Ok(match loc {
            Loc::Var(v, _) => Val::Scalar(self.builder.use_var(v)),
            Loc::Value(local, ty) => match self.values[local as usize] {
                Some(v) => Val::Scalar(v),
                None => {
                    // Read before its assignment in translation order: this attempt is
                    // discarded (see the module docs), so any value of the right type will do.
                    self.demote.push(local);
                    Val::Scalar(self.zero(scalar_type(ty)))
                }
            },
            Loc::Mem(base, offset, ty) => match ty {
                Ty::Agg(_) => Val::Agg(self.addr(base, offset)),
                Ty::Unit => Val::Unit,
                scalar => {
                    let t = scalar_type(scalar);
                    Val::Scalar(self.builder.ins().load(t, mem_flags(), base, offset))
                }
            },
            Loc::Unit => Val::Unit,
        })
    }

    /// A zero of type `t`.
    fn zero(&mut self, t: ir::Type) -> ir::Value {
        if t == types::F32 {
            self.builder.ins().f32const(0.0)
        } else if t == types::F64 {
            self.builder.ins().f64const(0.0)
        } else {
            self.builder.ins().iconst(t, 0)
        }
    }

    fn check_scalar_type(&self, v: ir::Value, ty: Ty) -> CodegenResult<()> {
        let have = self.builder.func.dfg.value_type(v);
        match cl_type(ty) {
            Some(want) if want == have => Ok(()),
            _ => bail!("type mismatch: cannot store a {have} value into a place of type {ty:?}"),
        }
    }

    pub(super) fn write(&mut self, loc: Loc, val: Val) -> CodegenResult<()> {
        match (loc, val) {
            (Loc::Var(v, ty), Val::Scalar(x)) => {
                self.check_scalar_type(x, ty)?;
                self.builder.def_var(v, x);
            }
            (Loc::Value(local, ty), Val::Scalar(x)) => {
                self.check_scalar_type(x, ty)?;
                if self.values[local as usize].replace(x).is_some() {
                    self.demote.push(local);
                }
            }
            (Loc::Mem(base, offset, Ty::Agg(id)), Val::Agg(src)) => {
                let (size, align) = size_align(self.program, Ty::Agg(id))?;
                let dst = self.addr(base, offset);
                // May alias (e.g. `x = x`), so allow overlap.
                self.copy(dst, src, u64::from(size), align, false);
            }
            (Loc::Mem(base, offset, ty), Val::Scalar(x)) => {
                self.check_scalar_type(x, ty)?;
                self.builder.ins().store(mem_flags(), x, base, offset);
            }
            (Loc::Unit, _) | (_, Val::Unit) => {}
            (l, _) => bail!("value/place kind mismatch for a place of type {:?}", l.ty()),
        }
        Ok(())
    }

    /// Copy `size` bytes; small copies are inlined, large ones call memcpy/memmove.
    pub(super) fn copy(
        &mut self,
        dst: ir::Value,
        src: ir::Value,
        size: u64,
        align: u32,
        non_overlapping: bool,
    ) {
        if size == 0 {
            return;
        }
        // `emit_small_memory_copy` requires align <= the largest power of two dividing `size`.
        let size_pow2 = 1u64 << size.trailing_zeros().min(7);
        let align = u64::from(align.max(1)).min(size_pow2) as u8;
        self.builder.emit_small_memory_copy(
            self.frontend_config,
            dst,
            src,
            size,
            align,
            align,
            non_overlapping,
            mem_flags(),
        );
    }
}
