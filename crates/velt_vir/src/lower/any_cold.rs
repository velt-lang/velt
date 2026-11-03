//! The `TypeError`s of the operations through `any` written at each site (member reads, writes
//! and calls, indexing): a `null` or `undefined` receiver, and a member the operation does not
//! reach yet (#924). Each message is the site's text around the key, so written out it was two
//! string concatenations and a panic per message, and the receiver check had one message for
//! `undefined` and one for `null`: about eight blocks per check at every site (#2189).
//!
//! The site now keeps the tests and calls a shared function that never returns:
//! - `Work::PanicKey`, `(before: ptr, key: ptr, after: ptr)`: stops with `before`, the key and
//!   `after` (static string objects for the site's two texts);
//! - `Work::AnyNullish(setting)`, `(tag: u64, key: ptr)`: Node's `Cannot read properties of
//!   undefined (reading 'k')` for the `typeof` tag of the receiver, `undefined` or `null`
//!   (`setting`: `Cannot set properties of … (setting 'k')`).

use super::any::{TAG_NULL, TAG_UNDEFINED};
use super::any_member::NullKey;
use super::rt::Rt;
use super::{cint, Cx, FnLower, Work};
use crate::vir::{BinOp, Callee, Const, Function, Operand, Place, Rvalue, Terminator, Ty, STR_AGG};

impl<'c, 'h> FnLower<'c, 'h> {
    /// Stop with `before` + the string at address `key` + `after`: one call of
    /// `Work::PanicKey` (module docs).
    pub(super) fn panic_with_key(&mut self, before: &str, key: &Operand, after: &str) {
        if self.dead() {
            return;
        }
        let b = self.cx.static_str_object(before);
        let a = self.cx.static_str_object(after);
        let f = self.cx.func(Work::PanicKey);
        let args = vec![
            Operand::Const(Const::Static(b), Ty::Ptr),
            key.clone(),
            Operand::Const(Const::Static(a), Ty::Ptr),
        ];
        self.call(Callee::Func(f), args, None, true);
    }

    /// `TypeError: Cannot read properties of undefined (reading 'k')` (Node's text) when the
    /// descriptor `desc` holds `null` / `undefined`; `key` is the key string's address, or a
    /// number key (formatted only on that path). `setting`: for a write (`Cannot set
    /// properties of undefined (setting 'k')`). The message is `Work::AnyNullish`'s.
    pub(super) fn any_nullish_check_as(&mut self, desc: &Operand, key: NullKey<'_>, setting: bool) {
        let tag = self.any_tag_of(desc.clone());
        let one = cint(TAG_NULL as i128, Ty::U64);
        let nil = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Le, tag.clone(), one));
        let (bad, ok) = (self.new_block(), self.new_block());
        self.branch(nil, bad, ok);
        self.switch_to(bad);
        let k = match key {
            NullKey::Str(k) => k.clone(),
            NullKey::Num(i) => {
                let ks = self.temp(Ty::Agg(STR_AGG));
                let ka = self.addr(Place::local(ks));
                self.call_rt(Rt::StrFromF64, vec![i.clone(), ka.clone()], None);
                ka
            }
        };
        let f = self.cx.func(Work::AnyNullish(setting));
        self.call(Callee::Func(f), vec![tag, k], None, true);
        self.switch_to(ok);
    }

    /// `Work::PanicKey` (module docs).
    pub(super) fn build_panic_key(cx: &'c mut Cx<'h>) -> Function {
        let mut lw = FnLower::bare(cx, vec![]);
        let before = lw.new_local(Ty::Ptr, Some("before".into()));
        let key = lw.new_local(Ty::Ptr, Some("key".into()));
        let after = lw.new_local(Ty::Ptr, Some("after".into()));
        let s = Ty::Agg(STR_AGG);
        let ab = lw.temp(s);
        let abp = lw.addr(Place::local(ab));
        let b = Operand::Copy(Place::local(before));
        let k = Operand::Copy(Place::local(key));
        lw.call_rt(Rt::StrConcat, vec![b, k, abp.clone()], None);
        let all = lw.temp(s);
        let allp = lw.addr(Place::local(all));
        let a = Operand::Copy(Place::local(after));
        lw.call_rt(Rt::StrConcat, vec![abp, a, allp.clone()], None);
        lw.call_rt(Rt::Panic, vec![allp], None);
        lw.terminate(Terminator::Unreachable);
        lw.finish("_Gpanic_key".into(), vec![Ty::Ptr; 3], Ty::Unit)
    }

    /// `Work::AnyNullish(setting)` (module docs).
    pub(super) fn build_any_nullish(cx: &'c mut Cx<'h>, setting: bool) -> Function {
        let mut lw = FnLower::bare(cx, vec![]);
        let tag = lw.new_local(Ty::U64, Some("tag".into()));
        let key = lw.new_local(Ty::Ptr, Some("key".into()));
        let undef = cint(TAG_UNDEFINED as i128, Ty::U64);
        let is_undef = lw.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, Operand::Copy(Place::local(tag)), undef),
        );
        let (u, n) = (lw.new_block(), lw.new_block());
        lw.branch(is_undef, u, n);
        let (verb, ing) = match setting {
            true => ("set", "setting"),
            false => ("read", "reading"),
        };
        let k = Operand::Copy(Place::local(key));
        for (b, word) in [(u, "undefined"), (n, "null")] {
            lw.switch_to(b);
            let head = format!("TypeError: Cannot {verb} properties of {word} ({ing} '");
            lw.panic_with_key(&head, &k, "')");
            if !lw.dead() {
                lw.terminate(Terminator::Unreachable);
            }
        }
        let name = match setting {
            true => "_Gany_nullish_set",
            false => "_Gany_nullish_get",
        };
        lw.finish(name.into(), vec![Ty::U64, Ty::Ptr], Ty::Unit)
    }
}

