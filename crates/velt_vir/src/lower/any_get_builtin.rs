//! `x.k` through `any` on an array, a string, a number, a boolean or a promise
//! (`Intrinsic::AnyGet`, any_glue.rs): a key that names none of the value's properties is
//! `undefined`, as in Node (`[1, 2].type`, `"s".kind`, `p.foo`). A promise's
//! `[Symbol.toStringTag]` is `"Promise"`; its `then`, `catch` and `finally` are read through
//! sema (`any_promise_members.rs`). A member of the prototype (a method, `[Symbol.iterator]`), one
//! of `Object.prototype`'s and an element index stay `ST_UNSUPPORTED`: a method or an element
//! read through `any` comes later (#924). The tables are Node's
//! `Reflect.ownKeys(String.prototype)` and so on, without `length` and `Object.prototype`'s
//! names (`scripts/any-has-tables.mjs` checks them).

use velt_sema::hir::{Nothing, TyId, TyKind};

use super::any_has_builtin::PROMISE_PROTO;
use super::any_member::{ST_OK, ST_UNSUPPORTED};
use super::{cint, FnLower};
use crate::vir::{self, BinOp, Operand, Place, Rvalue, Ty};
use velt_sema::js_protos::{ARRAY_PROTO, NUMBER_PROTO, OBJECT_PROTO, STRING_PROTO};

/// The key text of `[Symbol.toStringTag]`.
const TO_STRING_TAG: &str = "[Symbol.toStringTag]";

/// The names of an array, string, number, boolean or promise of type `ty` that a read through
/// `any` does not reach yet (a promise's tag is read: module docs), and whether its element
/// indexes are among them; `None` for other types.
fn builtin_names(kind: &TyKind) -> Option<(&'static [&'static str], bool)> {
    Some(match kind {
        TyKind::Array(_) => (ARRAY_PROTO, true),
        TyKind::Str => (STRING_PROTO, true),
        TyKind::Int(_) | TyKind::Float(_) => (NUMBER_PROTO, false),
        // `Boolean.prototype` has only `Object.prototype`'s names.
        TyKind::Bool => (&[], false),
        TyKind::Promise(..) => (PROMISE_PROTO, false),
        _ => return None,
    })
}

impl FnLower<'_, '_> {
    /// For a `ty` that [`builtin_names`] knows: `ST_UNSUPPORTED` for the keys it lists (and
    /// `Object.prototype`'s, and an element index), else `undefined` into `out`. `false`, with
    /// nothing emitted, for another type.
    pub(super) fn builtin_get_rest(&mut self, key: vir::Local, out: &Place, ty: TyId) -> bool {
        let Some((names, indexed)) = builtin_names(&self.cx.kind(ty)) else {
            return false;
        };
        if indexed {
            self.digit_key_unsupported(key);
        }
        if matches!(self.cx.kind(ty), TyKind::Promise(..)) {
            let next = self.on_key(key, TO_STRING_TAG);
            let s = self.str_lit("Promise");
            let str_ = self.cx.str_ty();
            self.any_box_into(s, str_, out);
            self.ret_status(ST_OK);
            self.switch_to(next);
        }
        for name in names.iter().chain(OBJECT_PROTO.iter()) {
            if *name == TO_STRING_TAG {
                continue;
            }
            let next = self.on_key(key, name);
            self.ret_status(ST_UNSUPPORTED);
            self.switch_to(next);
        }
        self.any_nothing_into(Nothing::Undefined, out);
        self.ret_status(ST_OK);
        true
    }

    /// `ST_UNSUPPORTED` for a key that starts with a digit (an element index, or no property:
    /// no member name starts with one); continues in the block for another key.
    fn digit_key_unsupported(&mut self, key: vir::Local) {
        let c = self.str_lead_bytes(&Operand::Copy(Place::local(key)), 1);
        let off = Rvalue::Binary(BinOp::Sub, c, cint(b'0' as i128, Ty::I64));
        let off = self.rvalue_temp(Ty::I64, off);
        let off = self.rvalue_temp(Ty::U64, Rvalue::Cast(off, Ty::U64));
        let digit = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Lt, off, cint(10, Ty::U64)));
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(digit, yes, no);
        self.switch_to(yes);
        self.ret_status(ST_UNSUPPORTED);
        self.switch_to(no);
    }
}
