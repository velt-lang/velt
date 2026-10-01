//! VIR types → LLVM IR types and C-ABI parameter attributes; spelling of constants, symbols
//! and aggregate layout checks.
//!
//! `Bool` is an `i8` holding 0/1 everywhere (registers and memory), like in the Cranelift
//! backend; comparisons produce `i1`, which is widened with `zext` right away. `Ptr` is LLVM's
//! opaque `ptr`, so alias analysis sees real pointers rather than integers.

use velt_vir::vir::{self, AggId, AggLayout, Ty};

use crate::CodegenResult;

/// LLVM type of a scalar VIR type; `None` for `Unit`/`Agg`.
pub(crate) fn llvm_type(ty: Ty) -> Option<&'static str> {
    Some(match ty {
        Ty::I8 | Ty::U8 | Ty::Bool => "i8",
        Ty::I16 | Ty::U16 => "i16",
        Ty::I32 | Ty::U32 => "i32",
        Ty::I64 | Ty::U64 => "i64",
        Ty::Ptr => "ptr",
        Ty::F32 => "float",
        Ty::F64 => "double",
        Ty::Unit | Ty::Agg(_) => return None,
    })
}

/// LLVM type of a type already known to be scalar.
pub(crate) fn scalar_type(ty: Ty) -> &'static str {
    llvm_type(ty).expect("ICE: scalar_type called on a non-scalar type")
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

/// Bit width of an integer-like type (`Ptr` = 64, `Bool` = 8); 0 otherwise.
pub(crate) fn int_bits(ty: Ty) -> u32 {
    as_int(ty)
        .and_then(Ty::scalar_size)
        .map_or(0, |bytes| bytes * 8)
}

/// Signature type with its C-ABI extension attribute: sub-32-bit integers are sign/zero
/// extended by the caller (clang's `signext`/`zeroext`), matching the Cranelift backend.
pub(crate) fn abi_type(ty: Ty) -> CodegenResult<String> {
    let Some(t) = llvm_type(ty) else {
        bail!("type {ty:?} is not allowed in a signature")
    };
    Ok(match ty {
        Ty::I8 | Ty::I16 => format!("{t} signext"),
        Ty::U8 | Ty::U16 | Ty::Bool => format!("{t} zeroext"),
        _ => t.to_string(),
    })
}

/// Return type of a signature (`void` for `Unit`).
pub(crate) fn abi_ret(ty: Ty) -> CodegenResult<String> {
    match ty {
        Ty::Unit => Ok("void".into()),
        Ty::I8 | Ty::I16 => Ok(format!("signext {}", scalar_type(ty))),
        Ty::U8 | Ty::U16 | Ty::Bool => Ok(format!("zeroext {}", scalar_type(ty))),
        _ => abi_type(ty),
    }
}

/// Integer constant of `ty` truncated to its width, printed as a signed decimal (LLVM's
/// canonical spelling; e.g. `255u8` is `-1` as an `i8`).
pub(crate) fn int_literal(ty: Ty, v: i128) -> String {
    let bits = int_bits(ty);
    if bits == 0 || bits >= 128 {
        return v.to_string();
    }
    let shift = 128 - bits;
    ((v << shift) >> shift).to_string()
}

/// Float constant as LLVM's exact hex form (an `f32` value is spelled as the equal `double`).
pub(crate) fn float_literal(ty: Ty, v: f64) -> String {
    let v = if ty == Ty::F32 {
        f64::from(v as f32)
    } else {
        v
    };
    format!("0x{:016X}", v.to_bits())
}

/// `@"symbol"`, escaping quotes, backslashes and non-printable bytes.
pub(crate) fn global_name(symbol: &str) -> String {
    format!("@\"{}\"", escape_bytes(symbol.as_bytes()))
}

/// Bytes for an LLVM `c"..."` string or quoted name.
pub(crate) fn escape_bytes(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len());
    for &b in bytes {
        if (0x20..0x7f).contains(&b) && b != b'"' && b != b'\\' {
            s.push(b as char);
        } else {
            s.push_str(&format!("\\{b:02X}"));
        }
    }
    s
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
            let n = scalar.scalar_size().unwrap_or(1);
            Ok((n, n))
        }
    }
}

/// Alignment known for `base_align`-aligned memory at byte `offset`.
pub(crate) fn offset_align(base_align: u32, offset: u64) -> u32 {
    if offset == 0 {
        return base_align.max(1);
    }
    let offset_pow2 = 1u64 << offset.trailing_zeros().min(31);
    u64::from(base_align.max(1)).min(offset_pow2) as u32
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals() {
        assert_eq!(int_literal(Ty::U8, 255), "-1");
        assert_eq!(int_literal(Ty::U8, 300), "44");
        assert_eq!(int_literal(Ty::I64, i128::from(u64::MAX)), "-1");
        assert_eq!(int_literal(Ty::I32, -5), "-5");
        assert_eq!(int_literal(Ty::Bool, 1), "1");
        assert_eq!(float_literal(Ty::F64, 1.0), "0x3FF0000000000000");
        assert_eq!(
            float_literal(Ty::F32, 0.1),
            format!("0x{:016X}", f64::from(0.1f32).to_bits())
        );
        assert_eq!(global_name("a\"b\n"), "@\"a\\22b\\0A\"");
    }

    #[test]
    fn alignment_at_offsets() {
        assert_eq!(offset_align(8, 0), 8);
        assert_eq!(offset_align(8, 4), 4);
        assert_eq!(offset_align(8, 24), 8);
        assert_eq!(offset_align(4, 1), 1);
    }

    #[test]
    fn abi_attributes() {
        assert_eq!(abi_type(Ty::I8).unwrap(), "i8 signext");
        assert_eq!(abi_type(Ty::Bool).unwrap(), "i8 zeroext");
        assert_eq!(abi_ret(Ty::U16).unwrap(), "zeroext i16");
        assert_eq!(abi_ret(Ty::Unit).unwrap(), "void");
        assert!(abi_type(Ty::Agg(AggId(0))).is_err());
    }
}
