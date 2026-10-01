//! Numeric casts with Rust `as` semantics: int↔int truncate/extend by *source* signedness,
//! int→float, float→int saturating (NaN → 0, via `llvm.fpto[su]i.sat`), f32↔f64, Bool→int,
//! int→Bool (`!= 0`), int↔Ptr.

use velt_vir::vir::Ty;

use super::Emitter;
use crate::types::{as_int, int_bits, llvm_type, scalar_type};
use crate::CodegenResult;

impl Emitter<'_> {
    pub(super) fn cast(&mut self, v: &str, from: Ty, to: Ty) -> CodegenResult<String> {
        if from == to {
            return Ok(v.to_string());
        }
        if llvm_type(to).is_none() {
            bail!("cannot cast to {to:?}")
        }
        if let Some(from_int) = as_int(from) {
            return Ok(self.cast_from_int(v, from, from_int, to));
        }
        if from.is_float() {
            return self.cast_from_float(v, from, to);
        }
        bail!("cannot cast {from:?} to {to:?}")
    }

    /// `from_int` is the integer view of `from` (`Ptr` → U64, `Bool` → U8).
    fn cast_from_int(&mut self, v: &str, from: Ty, from_int: Ty, to: Ty) -> String {
        let signed = from_int.is_signed();
        if to == Ty::Bool {
            let c = if from == Ty::Ptr {
                self.inst(format!("icmp ne ptr {v}, null"))
            } else {
                self.inst(format!("icmp ne {} {v}, 0", scalar_type(from)))
            };
            return self.bool_from_i1(&c);
        }
        if to == Ty::Ptr {
            let wide = self.int_resize(v, from, signed, 64);
            return self.inst(format!("inttoptr i64 {wide} to ptr"));
        }
        if as_int(to).is_some() {
            return self.int_resize(v, from, signed, int_bits(to));
        }
        let (v, from) = if from == Ty::Ptr {
            (self.inst(format!("ptrtoint ptr {v} to i64")), Ty::U64)
        } else {
            (v.to_string(), from)
        };
        let instr = if signed { "sitofp" } else { "uitofp" };
        self.inst(format!(
            "{instr} {} {v} to {}",
            scalar_type(from),
            scalar_type(to)
        ))
    }

    fn cast_from_float(&mut self, v: &str, from: Ty, to: Ty) -> CodegenResult<String> {
        let ft = scalar_type(from);
        if to == Ty::F64 {
            return Ok(self.inst(format!("fpext {ft} {v} to double")));
        }
        if to == Ty::F32 {
            return Ok(self.inst(format!("fptrunc {ft} {v} to float")));
        }
        let Some(to_int) = as_int(to).filter(|_| to != Ty::Bool) else {
            bail!("cannot cast a float to {to:?}")
        };
        let kind = if to_int.is_signed() {
            "fptosi"
        } else {
            "fptoui"
        };
        let it = format!("i{}", int_bits(to_int));
        let fname = if from == Ty::F32 { "f32" } else { "f64" };
        let name = format!("@llvm.{kind}.sat.{it}.{fname}");
        self.intrinsics.need(format!("declare {it} {name}({ft})"));
        let r = self.inst(format!("call {it} {name}({ft} {v})"));
        if to == Ty::Ptr {
            return Ok(self.inst(format!("inttoptr i64 {r} to ptr")));
        }
        Ok(r)
    }
}
