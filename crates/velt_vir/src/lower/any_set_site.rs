//! The call sites of `Intrinsic::AnySet` / `AnySetIndex` (the slots are any_set.rs's) and
//! `Intrinsic::AnyIsArray`.

use velt_sema::hir::{self, TyId};

use super::any::{TAG_NUMBER, TAG_STRING};
use super::any_member::{NullKey, ST_OK};
use super::any_set::{SLOT_SET, SLOT_SET_INDEX, ST_MISSING};
use super::operand::proj;
use super::{cint, FnLower};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// The class id word of the `any` table of an array or tuple type (glue/vtable.rs): no class
/// has it, so `instanceof` is false, and `Array.isArray` compares one word.
pub(super) const ARRAY_ID: u64 = u64::MAX - 1;

/// How a write's key reaches the slot.
#[derive(Clone)]
enum SetKey {
    /// The address of a string.
    Str(Operand),
    /// An `f64`.
    Num(Operand),
}

impl FnLower<'_, '_> {
    /// `AnySet` (`index`: `AnySetIndex`): `o[k] = v`. The receiver, the key and the value are
    /// evaluated in that order; a `null` / `undefined` receiver throws after them, as in
    /// JavaScript.
    pub(super) fn any_set(
        &mut self,
        o: &hir::Expr,
        k: &hir::Expr,
        v: &hir::Expr,
        index: bool,
    ) -> Operand {
        let unit = Operand::Const(crate::vir::Const::Unit, Ty::Unit);
        // A write must reach the object itself: an object type converted to `any` is then one
        // counted object, never a copy (`Cx::note_dyn_identity`).
        let any = self.cx.any_ty();
        self.cx.note_identity(any);
        self.cx.note_any_set();
        let (data, desc) = self.any_receiver(o);
        let kt = self.sub(k.ty);
        let kv = self.expr(k);
        // A number key by value; a string or `any` key by address.
        let kv = if index {
            self.rvalue_temp(Ty::F64, Rvalue::Use(kv))
        } else {
            let kp = self.place_of(kv, kt);
            self.addr(kp)
        };
        let vt = self.sub(v.ty);
        let vv = self.consume(v);
        if self.dead() {
            return unit;
        }
        let avt = self.cx.ty(vt);
        let buf = Place::local(self.temp(avt));
        self.assign(buf.clone(), Rvalue::Use(vv));
        let va = self.addr(buf);
        if index {
            self.any_set_at(&data, &desc, SetKey::Num(kv), va);
        } else if self.cx.is_dynamic(kt) {
            // An `any` key: a string names a member, a number an element.
            let kp = self.operand_place(kv, Ty::Ptr);
            let avt = self.cx.ty(kt);
            let kp = proj(&kp, Proj::Deref(avt));
            let kdata = Operand::Copy(proj(&kp, Proj::Field(0)));
            let kdesc = Operand::Copy(proj(&kp, Proj::Field(1)));
            let ktag = self.any_tag_of(kdesc);
            let (s_bb, n_bb, bad, done) = (
                self.new_block(),
                self.new_block(),
                self.new_block(),
                self.new_block(),
            );
            self.terminate(Terminator::Switch {
                value: ktag,
                cases: vec![(TAG_STRING as i128, s_bb), (TAG_NUMBER as i128, n_bb)],
                default: bad,
            });
            self.switch_to(s_bb);
            self.any_set_at(&data, &desc, SetKey::Str(kdata), va.clone());
            self.goto(done);
            self.switch_to(n_bb);
            let num = self.any_key_number(&kp);
            self.any_set_at(&data, &desc, SetKey::Num(num), va);
            self.goto(done);
            self.switch_to(bad);
            self.panic_msg(
                "TypeError: a key of this type is not supported through `any` yet (#924)",
            );
            self.terminate(Terminator::Unreachable);
            self.switch_to(done);
        } else {
            self.any_set_at(&data, &desc, SetKey::Str(kv), va);
        }
        unit
    }

    /// The null check, the slot call and the status check of a write with key `key`.
    fn any_set_at(&mut self, data: &Operand, desc: &Operand, key: SetKey, v: Operand) {
        let (st, k) = match &key {
            SetKey::Str(k) => {
                self.any_nullish_check_as(desc, NullKey::Str(k), true);
                let args = vec![data.clone(), k.clone(), v];
                let st = self.any_slot(desc.clone(), SLOT_SET, args, vec![Ty::Ptr; 3]);
                (st, Some(k))
            }
            SetKey::Num(i) => {
                self.any_nullish_check_as(desc, NullKey::Num(i), true);
                let args = vec![data.clone(), i.clone(), v];
                let p = vec![Ty::Ptr, Ty::F64, Ty::Ptr];
                (self.any_slot(desc.clone(), SLOT_SET_INDEX, args, p), None)
            }
        };
        let ok = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, st.clone(), cint(ST_OK, Ty::U8)),
        );
        let (good, bad) = (self.new_block(), self.new_block());
        self.branch(ok, good, bad);
        self.switch_to(bad);
        let missing = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, st, cint(ST_MISSING, Ty::U8)),
        );
        let (miss, other) = (self.new_block(), self.new_block());
        self.branch(missing, miss, other);
        self.switch_to(miss);
        let tail = ": an object of a fixed shape gets no new properties through `any` (#924)";
        match k {
            Some(k) => self.panic_with_key("TypeError: Cannot add property ", k, tail),
            None => self.panic_msg(&format!("TypeError: Cannot add an element{tail}")),
        }
        self.terminate(Terminator::Unreachable);
        self.switch_to(other);
        match k {
            Some(k) => self.panic_with_key(
                "TypeError: writing '",
                k,
                "' through `any` is not supported for this value yet (#924)",
            ),
            None => self.panic_msg(
                "TypeError: writing an element through `any` is not supported for this value yet (#924)",
            ),
        }
        self.terminate(Terminator::Unreachable);
        self.switch_to(good);
    }

    /// `Array.isArray(a)` on an `any`: its table's class id word is `ARRAY_ID`.
    pub(super) fn any_is_array(&mut self, a: &hir::Expr) -> Operand {
        let t: TyId = self.sub(a.ty);
        let v = self.expr(a);
        if self.dead() {
            return Operand::Const(crate::vir::Const::Bool(false), Ty::Bool);
        }
        let (_, desc) = self.any_parts(v, t);
        let id = self.vtable_class_id(desc);
        let want = cint(ARRAY_ID as i128, Ty::U64);
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, id, want))
    }
}
