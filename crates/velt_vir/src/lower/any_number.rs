//! Operators on `any` (#924; hir_encodings.md "any and unknown"): `Intrinsic::AnyTag`, the
//! `typeof` tag of the value held (a load from its descriptor, no call), and
//! `Intrinsic::AnyToNumber`, JavaScript's `ToNumber` of a primitive held: one indirect call
//! through descriptor slot 3. That slot is filled for booleans, numbers and strings in every
//! program (`==` uses it); in a program that converts an `any` to a number
//! (`Boxing::any_number`) every table has it: `null` is 0, `undefined` NaN, a symbol stops
//! with Node's `TypeError`, and an object or a function gives NaN (sema converts those with
//! `ToPrimitive` before, through the prelude's `__anyObjectNumber`).

use velt_sema::hir::{self, TyId, TyKind};

use super::any::SLOT_NUMBER;
use super::FnLower;
use crate::vir::{Const, Operand, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    /// `AnyTag(a)`: the tag word of the value's descriptor, as an `i64`.
    pub(super) fn any_tag_value(&mut self, a: &hir::Expr) -> Operand {
        let t = self.sub(a.ty);
        let v = self.expr(a);
        let (_, desc) = self.any_parts(v, t);
        let tag = self.any_tag_of(desc);
        self.rvalue_temp(Ty::I64, Rvalue::Cast(tag, Ty::I64))
    }

    /// `AnyToNumber(a)`: one call of the value's `ToNumber` slot.
    pub(super) fn any_to_number(&mut self, a: &hir::Expr) -> Operand {
        self.cx.note_any_number();
        let t = self.sub(a.ty);
        let v = self.expr(a);
        let (data, desc) = self.any_parts(v, t);
        self.any_slot_call(desc, SLOT_NUMBER, vec![data], Ty::F64)
    }

    /// The `ToNumber` slot of a type that is not a boolean, a number or a string (module docs).
    pub(super) fn any_number_other(&mut self, ty: TyId) {
        let nan = Operand::Const(Const::Float(f64::NAN), Ty::F64);
        let r = match self.cx.kind(ty) {
            // An option held is its nothing (a value is unpacked when boxed).
            TyKind::Option(_, n) if n.has_null() => Operand::Const(Const::Float(0.0), Ty::F64),
            TyKind::Symbol => {
                self.panic_msg("TypeError: Cannot convert a Symbol value to a number");
                return self.terminate(Terminator::Unreachable);
            }
            _ => nan,
        };
        self.terminate(Terminator::Return(r));
    }
}
