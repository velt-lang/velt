//! Operands: place reads, constants, and symbol addresses (functions, externs, statics).

use velt_vir::vir::{Const, Operand, Ty};

use super::{Emitter, Val};
use crate::statics::static_name;
use crate::types::{float_literal, global_name, int_literal, llvm_type};
use crate::CodegenResult;

impl Emitter<'_> {
    /// Evaluate an operand; aggregates evaluate to their location.
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
    pub(super) fn scalar(&mut self, op: &Operand) -> CodegenResult<(String, Ty)> {
        match self.operand(op)? {
            (Val::Scalar(v), ty) => Ok((v, ty)),
            (_, ty) => bail!("expected a scalar operand, found type {ty:?}"),
        }
    }

    fn constant(&self, c: &Const, ty: Ty) -> CodegenResult<Val> {
        if ty == Ty::Unit || *c == Const::Unit {
            return Ok(Val::Unit);
        }
        if llvm_type(ty).is_none() {
            bail!("constant of aggregate type {ty:?}")
        }
        let v = match c {
            // Convert straight to the target width (i128 → f64 → f32 could round twice).
            Const::Int(i) if ty == Ty::F32 => float_literal(ty, f64::from(*i as f32)),
            Const::Int(i) if ty == Ty::F64 => float_literal(ty, *i as f64),
            Const::Int(0) if ty == Ty::Ptr => "null".into(),
            Const::Int(i) if ty == Ty::Ptr => {
                format!("inttoptr (i64 {} to ptr)", int_literal(Ty::U64, *i))
            }
            Const::Int(i) => int_literal(ty, *i),
            Const::Float(x) if ty.is_float() => float_literal(ty, *x),
            Const::Float(_) => bail!("float constant with non-float type {ty:?}"),
            Const::Bool(_) if ty.is_float() => bail!("bool constant with float type {ty:?}"),
            Const::Bool(false) if ty == Ty::Ptr => "null".into(),
            Const::Bool(true) if ty == Ty::Ptr => "inttoptr (i64 1 to ptr)".into(),
            Const::Bool(b) => i32::from(*b).to_string(),
            Const::Static(_) | Const::Func(_) | Const::Extern(_) if ty != Ty::Ptr => {
                bail!("address constant must have type Ptr, found {ty:?}")
            }
            Const::Static(id) => {
                if id.0 as usize >= self.program.statics.len() {
                    bail!("unknown static #{}", id.0)
                }
                static_name(id.0 as usize)
            }
            Const::Func(id) => match self.program.funcs.get(id.0 as usize) {
                Some(f) => global_name(&f.symbol),
                None => bail!("unknown function #{}", id.0),
            },
            Const::Extern(id) => match self.program.externs.get(id.0 as usize) {
                Some(e) => global_name(&e.symbol),
                None => bail!("unknown extern #{}", id.0),
            },
            Const::Unit => unreachable!("ICE: handled above"),
        };
        Ok(Val::Scalar(v))
    }
}
