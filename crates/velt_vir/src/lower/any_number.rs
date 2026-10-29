//! Operators on `any` (#924; hir_encodings.md "any and unknown"): `Intrinsic::AnyTag`, the
//! `typeof` tag of the value held (a load from its descriptor, no call), and
//! `Intrinsic::AnyToNumber`, JavaScript's `ToNumber` of a primitive held: one indirect call
//! through descriptor slot 3. That slot is filled for booleans, numbers and strings in every
//! program (`==` uses it); in a program that converts an `any` to a number
//! (`Boxing::any_number`) every table has it: `null` is 0, `undefined` NaN, a symbol stops
//! with Node's `TypeError`, and an object or a function gives NaN (sema converts those with
//! `ToPrimitive` before, through the prelude's `__anyObjectNumber`).
//!
//! An `any` holding a number of another numeric type than the one it is converted to (an `i64`
//! read back as a `number`, a `number` as an `i64`, a generic `P = i64` boxed and converted to
//! `i64` where sema gave the box the `number` it is): JavaScript has one number, so the value
//! converts as `as` converts its value, not a `TypeError`: a float truncates and saturates
//! (`NaN` is 0), an integer wraps (`300 as u8` is 44), and a 64-bit integer is read as itself,
//! exact past 2^53.

use velt_sema::hir::{self, FloatTy, IntTy, TyId, TyKind};

use super::any::{SLOT_NUMBER, TAG_NUMBER};
use super::glue::{Glue, VtableKey, SLOT_DROP};
use super::{cint, FnLower};
use crate::vir::{
    BinOp, BlockId, Callee, Const, Local, Operand, Place, Proj, Rvalue, Terminator, Ty,
};

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

impl FnLower<'_, '_> {
    /// Where `any_unbox` found no value of the numeric type `ty` in the `any` at `p`
    /// (descriptor `desc`): a number of another numeric type converts (module docs) into `out`,
    /// going on to `done`. Returns whether the `TypeError` of anything else is raised here too;
    /// otherwise the code goes on in the current block, to the caller's.
    ///
    /// Every `any` member read or argument converted to a number has this path, so it costs one
    /// call per site: a glue per numeric type that converts and drops the `any`
    /// (`Glue::AnyNumericOrError`), and whose `TypeError` is a call that never returns
    /// (`Glue::AnyNumberError`), so that the optimizer keeps it cold when it inlines the glue
    /// into a loop. (Inline at every site, the conversion made the `Ganycall` glue a fifth
    /// larger; with a branch per site and the error in the glue, compiling
    /// `tests/golden/lang/any_view_object.vlt` took 16% more instructions.) A site that names
    /// what it converts (`any_context`) has its own message: it calls `Glue::AnyNumeric` and
    /// branches to it on `false`.
    pub(super) fn any_number_convert(
        &mut self,
        p: &Place,
        ty: TyId,
        desc: Operand,
        out: &Place,
        done: BlockId,
    ) -> bool {
        let vt = match self.cx.kind(ty) {
            TyKind::Int(_) | TyKind::Float(_) => self.cx.ty(ty),
            _ => return false,
        };
        let stops = self.any_context.is_none();
        let g = match stops {
            true => Glue::AnyNumericOrError,
            false => Glue::AnyNumeric,
        };
        let f = self.cx.late_glue(g, ty);
        let slot = Place::local(self.temp(vt));
        let args = vec![
            Operand::Copy(super::operand::proj(p, Proj::Field(0))),
            desc,
            self.rvalue_temp(Ty::Ptr, Rvalue::AddrOf(slot.clone())),
        ];
        let no = match stops {
            true => {
                self.call(Callee::Func(f), args, None, false);
                None
            }
            false => {
                let ok = Place::local(self.temp(Ty::Bool));
                self.call(Callee::Func(f), args, Some(ok.clone()), false);
                let (yes, no) = (self.new_block(), self.new_block());
                self.branch(Operand::Copy(ok), yes, no);
                self.switch_to(yes);
                Some(no)
            }
        };
        self.assign(out.clone(), Rvalue::Use(Operand::Copy(slot)));
        self.goto(done);
        if let Some(no) = no {
            self.switch_to(no);
        }
        stops
    }

    /// `Glue::AnyNumeric` of the numeric type `ty`: the number of another numeric type held by
    /// the `any` of data `data` and descriptor `desc`, converted to `ty` (module docs) and
    /// written to `out`, and `true`, the `any` dropped; `false` for a value that is not a
    /// number, the `any` kept. With `stops` (`Glue::AnyNumericOrError`), nothing is returned
    /// and a value that is not a number stops with the `TypeError`.
    pub(super) fn any_numeric_body(
        &mut self,
        data: Local,
        desc: Local,
        out: Local,
        ty: TyId,
        stops: bool,
    ) {
        let vt = self.cx.ty(ty);
        let dest = self.deref_param(out, ty);
        let (data, desc) = (
            Operand::Copy(Place::local(data)),
            Operand::Copy(Place::local(desc)),
        );
        // Every conversion ends here: the box is dropped (through its descriptor) and `true`.
        let yes = self.new_block();
        let tag = self.any_tag_of(desc.clone());
        let num = cint(TAG_NUMBER as i128, Ty::U64);
        let is_num = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, tag, num));
        let (conv, other) = (self.new_block(), self.new_block());
        self.branch(is_num, conv, other);
        self.switch_to(other);
        match stops {
            true => {
                // Out of line, where it never returns: the optimizer keeps such calls cold
                // and does not inline them with this glue into a loop.
                let f = self.cx.func(super::Work::Glue(Glue::AnyNumberError, ty));
                self.call(Callee::Func(f), vec![desc.clone()], None, true);
            }
            false => self.terminate(Terminator::Return(Operand::Const(
                Const::Bool(false),
                Ty::Bool,
            ))),
        }
        self.switch_to(conv);
        let imp = self.cx.hir.any_impl.unwrap_or(0);
        // Only the numeric types `any` values hold have a descriptor to compare with (naming
        // another's would make its table and glue): the glue is built last, and again when
        // more appear (`Cx::late_glue`).
        let held = |lw: &Self, k: TyKind| {
            (lw.cx.types.get(&k)).filter(|t| *t != ty && lw.cx.facts.dyn_types.contains(t))
        };
        // A 64-bit integer: its value, exact past 2^53, wrapping as `as` wraps.
        for k in [IntTy::I64, IntTy::U64, IntTy::ISize, IntTy::USize] {
            let Some(t) = held(self, TyKind::Int(k)) else {
                continue;
            };
            let want = self.vtable_addr(VtableKey::Impl(imp, t));
            let is = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, desc.clone(), want));
            let (hit, miss) = (self.new_block(), self.new_block());
            self.branch(is, hit, miss);
            self.switch_to(hit);
            let it = self.cx.ty(t);
            let dp = self.operand_place(data.clone(), Ty::Ptr);
            let i = Operand::Copy(super::operand::proj(&dp, Proj::Deref(it)));
            let i = self.copy_to_temp(i, it);
            let i = Operand::Copy(Place::local(i));
            let v = if vt == it {
                i
            } else {
                self.rvalue_temp(vt, Rvalue::Cast(i, vt))
            };
            self.assign(dest.clone(), Rvalue::Use(v));
            self.goto(yes);
            self.switch_to(miss);
        }
        let f = self.any_slot_call(desc.clone(), SLOT_NUMBER, vec![data.clone()], Ty::F64);
        let v = match vt {
            Ty::F64 => f,
            Ty::F32 => self.rvalue_temp(vt, Rvalue::Cast(f, vt)),
            _ => {
                // A float casts as `as` casts it (truncating, saturating: `NaN` is 0); a
                // narrower integer goes through `i64` (exact), so that it wraps as `300 as u8`.
                let fl = self.new_block();
                for k in [FloatTy::F64, FloatTy::F32] {
                    let Some(t) = held(self, TyKind::Float(k)) else {
                        continue;
                    };
                    let want = self.vtable_addr(VtableKey::Impl(imp, t));
                    let is =
                        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, desc.clone(), want));
                    let next = self.new_block();
                    self.branch(is, fl, next);
                    self.switch_to(next);
                }
                let i = self.rvalue_temp(Ty::I64, Rvalue::Cast(f.clone(), Ty::I64));
                let c = self.rvalue_temp(vt, Rvalue::Cast(i, vt));
                self.assign(dest.clone(), Rvalue::Use(c));
                self.goto(yes);
                self.switch_to(fl);
                self.rvalue_temp(vt, Rvalue::Cast(f, vt))
            }
        };
        self.assign(dest, Rvalue::Use(v));
        self.goto(yes);
        self.switch_to(yes);
        let drop = self.dispatch(desc, SLOT_DROP);
        self.call_entry(drop, vec![data], vec![Ty::Ptr], Ty::Unit);
        let r = match stops {
            true => Operand::Const(Const::Unit, Ty::Unit),
            false => Operand::Const(Const::Bool(true), Ty::Bool),
        };
        self.terminate(Terminator::Return(r));
    }
}
