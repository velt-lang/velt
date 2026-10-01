//! VIR types → Cranelift register types and C-ABI signatures, plus layout lookups.

use cranelift_codegen::ir::{self, types, AbiParam, Signature};
use cranelift_codegen::isa::CallConv;
use velt_vir::vir::{self, AggId, AggLayout, Ty};

use crate::CodegenResult;

/// Cranelift register type for a scalar VIR type (`Bool` is `i8`); `None` for `Unit`/`Agg`.
pub(crate) fn cl_type(ty: Ty) -> Option<ir::Type> {
    Some(match ty {
        Ty::I8 | Ty::U8 | Ty::Bool => types::I8,
        Ty::I16 | Ty::U16 => types::I16,
        Ty::I32 | Ty::U32 => types::I32,
        Ty::I64 | Ty::U64 | Ty::Ptr => types::I64,
        Ty::F32 => types::F32,
        Ty::F64 => types::F64,
        Ty::Unit | Ty::Agg(_) => return None,
    })
}

/// Cranelift type of a type already known to be scalar.
pub(crate) fn scalar_type(ty: Ty) -> ir::Type {
    cl_type(ty).expect("ICE: scalar_type called on a non-scalar type")
}

/// Bit width of a scalar type (0 for `Unit`/`Agg`).
pub(crate) fn int_bits(ty: Ty) -> u32 {
    cl_type(ty).map(|t| t.bits()).unwrap_or(0)
}

/// Normalize types for integer semantics: `Ptr` behaves like `U64`, `Bool` like `U8`.
pub(crate) fn as_int(ty: Ty) -> Option<Ty> {
    match ty {
        Ty::Ptr => Some(Ty::U64),
        Ty::Bool => Some(Ty::U8),
        t if t.is_int() => Some(t),
        _ => None,
    }
}

fn abi_param(ty: Ty) -> CodegenResult<AbiParam> {
    let Some(t) = cl_type(ty) else {
        bail!("type {ty:?} is not allowed in a signature")
    };
    // C ABI: sub-32-bit integers are extended (clang's signext/zeroext), so Rust/C callers
    // and callees agree on the upper bits.
    Ok(match ty {
        Ty::I8 | Ty::I16 => AbiParam::new(t).sext(),
        Ty::U8 | Ty::U16 | Ty::Bool => AbiParam::new(t).uext(),
        _ => AbiParam::new(t),
    })
}

/// C-ABI signature for scalar params and return (`Unit` return = no results).
pub(crate) fn make_signature(cc: CallConv, params: &[Ty], ret: Ty) -> CodegenResult<Signature> {
    let mut sig = Signature::new(cc);
    for &p in params {
        sig.params.push(abi_param(p)?);
    }
    if ret != Ty::Unit {
        sig.returns.push(abi_param(ret)?);
    }
    Ok(sig)
}

/// Layout of an aggregate, or an error for an unknown id.
pub(crate) fn aggregate(program: &vir::Program, id: AggId) -> CodegenResult<&AggLayout> {
    match program.aggs.get(id.0 as usize) {
        Some(a) => Ok(a),
        None => bail!("unknown aggregate #{}", id.0),
    }
}

/// Size and alignment of any type.
pub(crate) fn size_align(program: &vir::Program, ty: Ty) -> CodegenResult<(u32, u32)> {
    match ty {
        Ty::Agg(id) => {
            let layout = aggregate(program, id)?;
            Ok((layout.size, layout.align))
        }
        Ty::Unit => Ok((0, 1)),
        scalar => {
            let n = scalar.scalar_size().unwrap_or(0);
            Ok((n, n))
        }
    }
}

/// Check aggregate layouts: power-of-two alignment, known nested aggregates, fields in bounds.
pub(crate) fn validate_aggregates(program: &vir::Program) -> CodegenResult<()> {
    for (i, a) in program.aggs.iter().enumerate() {
        if a.align == 0 || !a.align.is_power_of_two() {
            bail!(
                "codegen: aggregate #{i} `{}`: alignment {} is not a power of two",
                a.name,
                a.align
            );
        }
        for (field, (field_ty, offset)) in a.fields.iter().enumerate() {
            let (field_size, _) = size_align(program, *field_ty)
                .map_err(|e| format!("codegen: aggregate #{i} `{}` field {field}: {e}", a.name))?;
            if u64::from(*offset) + u64::from(field_size) > u64::from(a.size) {
                bail!(
                    "codegen: aggregate #{i} `{}` field {field} (offset {offset}) exceeds its size {}",
                    a.name,
                    a.size
                );
            }
        }
    }
    Ok(())
}
