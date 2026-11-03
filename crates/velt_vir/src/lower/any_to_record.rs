//! `AnyTo` a `Record<string, V>` from an `any` holding an object that is not that very record
//! (#1219): an object literal, a class instance, an array, or a record of other values
//! (`style: { color }` passed through `any` to a `Record<string, any>` parameter). Node reads any
//! object as a record. Velt's record is its own type, so the conversion builds one: std's hook
//! `anyToRecordAny(o)` (`V` is `any`) or `anyToRecord<V>(o)` (std/lang/hooks/any_record.vlt,
//! loaded for a program that mentions `Record` and checked for one that may convert to it,
//! velt_sema `lazy_std::RECORD_HOOKS`; without it the conversion stays the `TypeError`) makes a
//! new record of the object's own enumerable entries, in its order (`Object.keys`, which asks a
//! proxy's traps), each value converted to `V` as `AnyTo` converts it. It is a copy, as an
//! object converted to another object type is (any_object_fill.rs): a later write through one is
//! not seen through the other. `null`, `undefined` and primitives stay a `TypeError`.

use velt_sema::hir::{DefId, PassMode, TyId, TyKind};

use super::any::TAG_OBJECT;
use super::any_glue::Ready;
use super::operand::proj;
use super::{cint, FnLower};
use crate::vir::{BinOp, BlockId, Callee, Operand, Place, Proj, Rvalue, Ty};

/// The hook for a `Record<string, any>`.
const TO_RECORD_ANY: &str = velt_common::std_hook!(any_to_record_any);
/// The hook for a `Record<string, V>` of another `V`.
const TO_RECORD: &str = velt_common::std_hook!(any_to_record);

impl FnLower<'_, '_> {
    /// Where the `any` at `p` (of type `from`, owned) holds an object and `ty` is a record with
    /// string keys: the record built from its entries (module docs) into `out`, then on to
    /// `done`. Any other value goes on in the current block.
    pub(super) fn any_to_record(
        &mut self,
        p: &Place,
        from: TyId,
        ty: TyId,
        out: &Place,
        done: BlockId,
    ) {
        let Some((k, v)) = self.prelude_record(ty) else {
            return;
        };
        if !matches!(self.cx.kind(k), TyKind::Str) {
            return;
        }
        let (name, targs) = match v == self.cx.any_ty() {
            true => (TO_RECORD_ANY, vec![]),
            false => (TO_RECORD, vec![v]),
        };
        // sema checks the hooks, and adds the member slots of the `any` tables they read, only
        // for a program that may convert to a record (velt_sema `lazy_std::RECORD_HOOKS`);
        // without them the conversion stays the `TypeError`.
        let hook = self.cx.prelude_def(name);
        let usable =
            |d: &DefId| !self.cx.hir.unchecked.contains(d) && self.cx.hir.dyn_members.is_some();
        let Some(d) = hook.filter(usable) else {
            return;
        };
        let desc = Operand::Copy(proj(p, Proj::Field(1)));
        let tag = self.any_tag_of(desc);
        let obj = cint(TAG_OBJECT as i128, Ty::U64);
        let is_obj = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, tag, obj));
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(is_obj, yes, no);
        self.switch_to(yes);
        let owned = self.cx.fn_def(d).params[0].mode == PassMode::Owned;
        let arg = self.addr(p.clone());
        let r = self.call_std_hook(d, targs, vec![arg]);
        if !owned {
            self.drop_glue(p.clone(), from);
        }
        self.assign(out.clone(), Rvalue::Use(r));
        self.goto(done);
        self.switch_to(no);
    }

    /// A direct call of std's hook `def` (instantiated with `targs`) with the arguments `argv`
    /// as its parameters' modes take them: its result, an owned value this function no longer
    /// drops (the caller moves it on). As `invoke_ready` calls std from glue: a hook that reports
    /// its caller's location reports none there, and an error it throws (a proxy's trap, through
    /// a spread) is uncaught.
    pub(super) fn call_std_hook(
        &mut self,
        def: DefId,
        targs: Vec<TyId>,
        argv: Vec<Operand>,
    ) -> Operand {
        let f = match self.panic_loc() {
            None if self.cx.tracks_caller(def) => self.cx.func(super::Work::Tracked(
                def,
                targs.clone(),
                super::srcloc::UNKNOWN_LOC,
            )),
            _ => self.cx.func_for_direct(def, targs.clone(), self.mode),
        };
        let fd = self.cx.fn_def(def);
        let (ret, throws) = self.cx.call_sig(fd);
        let ret = self.cx.subst(ret, &targs);
        let r = Ready {
            def,
            targs: targs.clone(),
            ret,
            throws: throws.map(|t| self.cx.subst(t, &targs)),
        };
        let v = self.finish_ready_call(Callee::Func(f), argv, &r);
        if let Operand::Copy(p) = &v {
            self.take_temp(&p.clone());
        }
        v
    }
}
