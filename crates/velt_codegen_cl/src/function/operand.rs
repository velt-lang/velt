//! Operands: place reads, constants, and symbol references (functions, externs, statics).

use cranelift_codegen::ir::{self, types, InstBuilder};
use cranelift_module::{FuncId, Module};
use velt_vir::vir::{self, Const, Operand, StaticId, Ty};

use super::{Translator, Val};
use crate::abi::cl_type;
use crate::CodegenResult;

impl<M: Module> Translator<'_, '_, M> {
    /// Evaluate an operand; aggregates evaluate to their address.
    pub(super) fn operand(&mut self, op: &Operand) -> CodegenResult<(Val, Ty)> {
        match op {
            Operand::Copy(p) => {
                let loc = self.place(p)?;
                let ty = loc.ty();
                Ok((self.read(loc)?, ty))
            }
            Operand::Const(c, ty) => Ok((self.constant(c, *ty)?, *ty)),
        }
    }

    /// Evaluate an operand that must be a scalar.
    pub(super) fn scalar(&mut self, op: &Operand) -> CodegenResult<(ir::Value, Ty)> {
        match self.operand(op)? {
            (Val::Scalar(v), ty) => Ok((v, ty)),
            (_, ty) => bail!("expected a scalar operand, found type {ty:?}"),
        }
    }

    /// Integer constant, truncated to the type's width (Cranelift wants zero-extended
    /// immediates for narrow types).
    pub(super) fn iconst(&mut self, ty: ir::Type, v: i128) -> ir::Value {
        let bits = ty.bits();
        let v = if bits >= 64 {
            v as i64
        } else {
            (v as i64) & ((1i64 << bits) - 1)
        };
        self.builder.ins().iconst(ty, v)
    }

    fn constant(&mut self, c: &Const, ty: Ty) -> CodegenResult<Val> {
        if ty == Ty::Unit || *c == Const::Unit {
            return Ok(Val::Unit);
        }
        let Some(t) = cl_type(ty) else {
            bail!("constant of aggregate type {ty:?}")
        };
        let v = match c {
            Const::Int(i) if ty == Ty::F32 => self.builder.ins().f32const(*i as f32),
            Const::Int(i) if ty == Ty::F64 => self.builder.ins().f64const(*i as f64),
            Const::Int(i) => self.iconst(t, *i),
            Const::Float(x) if ty == Ty::F32 => self.builder.ins().f32const(*x as f32),
            Const::Float(x) if ty == Ty::F64 => self.builder.ins().f64const(*x),
            Const::Float(_) => bail!("float constant with non-float type {ty:?}"),
            Const::Bool(_) if ty.is_float() => bail!("bool constant with float type {ty:?}"),
            Const::Bool(b) => self.iconst(t, i128::from(*b)),
            Const::Static(_) | Const::Func(_) | Const::Extern(_) if ty != Ty::Ptr => {
                bail!("address constant must have type Ptr, found {ty:?}")
            }
            Const::Static(id) => self.static_addr(*id)?,
            Const::Func(id) => {
                let func = self.func_id(*id)?;
                self.func_addr(func)
            }
            Const::Extern(id) => {
                let func = self.extern_id(*id)?;
                self.func_addr(func)
            }
            Const::Unit => unreachable!("ICE: handled above"),
        };
        Ok(Val::Scalar(v))
    }

    fn static_addr(&mut self, id: StaticId) -> CodegenResult<ir::Value> {
        let Some(&data) = self.decls.statics.get(id.0 as usize) else {
            bail!("unknown static #{}", id.0)
        };
        let gv = match self.data_refs.get(&data) {
            Some(g) => *g,
            None => {
                let g = self.module.declare_data_in_func(data, self.builder.func);
                if self.far_addresses {
                    if let ir::GlobalValueData::Symbol { colocated, .. } =
                        &mut self.builder.func.global_values[g]
                    {
                        *colocated = false;
                    }
                }
                self.data_refs.insert(data, g);
                g
            }
        };
        Ok(self.builder.ins().symbol_value(types::I64, gv))
    }

    fn func_addr(&mut self, func: FuncId) -> ir::Value {
        let r = self.addr_ref(func);
        self.builder.ins().func_addr(types::I64, r)
    }

    /// Function reference for direct calls.
    pub(super) fn func_ref(&mut self, id: FuncId) -> ir::FuncRef {
        if let Some(r) = self.func_refs.get(&id) {
            return *r;
        }
        let r = self.module.declare_func_in_func(id, self.builder.func);
        self.func_refs.insert(id, r);
        r
    }

    /// Function reference for `func_addr` (non-colocated when `far_addresses`).
    fn addr_ref(&mut self, id: FuncId) -> ir::FuncRef {
        if !self.far_addresses {
            return self.func_ref(id);
        }
        if let Some(r) = self.addr_refs.get(&id) {
            return *r;
        }
        let r = self.module.declare_func_in_func(id, self.builder.func);
        self.builder.func.dfg.ext_funcs[r].colocated = false;
        self.addr_refs.insert(id, r);
        r
    }

    pub(super) fn func_id(&self, id: vir::FuncId) -> CodegenResult<FuncId> {
        match self.decls.funcs.get(id.0 as usize) {
            Some(x) => Ok(*x),
            None => bail!("unknown function #{}", id.0),
        }
    }

    pub(super) fn extern_id(&self, id: vir::ExternId) -> CodegenResult<FuncId> {
        match self.decls.externs.get(id.0 as usize) {
            Some(x) => Ok(*x),
            None => bail!("unknown extern #{}", id.0),
        }
    }
}
