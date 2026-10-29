//! Properties of function values through `any` (#924; rt_abi.md "Function properties"): as in
//! JavaScript, a function is an object a program may add properties to
//! (`factory.__setup = setup`). The member slots of a function type's `any` table (any_glue.rs,
//! any_set.rs, any_has.rs, any_keys.rs, any_delete.rs) look a key up in the runtime's table of
//! the function's properties, keyed by its closure environment, which is the function's
//! identity: a program that writes through `any` and holds functions there gives every closure
//! a counted environment (`Boxing::fn_identity`), so copies of the function share it, and its
//! properties; a function value without one (a declared function's) has the identity the
//! runtime keeps for its code, made at the first property write to any reference to it.
//!
//! A property read or call that finds nothing goes on as before: the built-in members of
//! functions (`name`, `call`, ...) are not supported yet, and another key is `undefined`, as in
//! Node. Writing `name` or `length` (read-only in JavaScript) is not supported either. In a
//! program without writes through `any` no function has properties, and the slots are the ones
//! they were before.

use velt_sema::hir::{Nothing, TyId, TyKind};

use super::any_glue::OBJECT_PROTO;
use super::any_member::{SLOT_APPLY, ST_OK, ST_UNSUPPORTED};
use super::operand::proj;
use super::rt::Rt;
use super::{cint, FnLower};
use crate::vir::{self, BlockId, Operand, Place, Proj, Rvalue, Terminator, Ty, STR_AGG};

/// What every function has from JavaScript itself or `Function.prototype`, not supported
/// through `any` yet.
const FN_PROTO: [&str; 8] = [
    "name",
    "length",
    "prototype",
    "call",
    "apply",
    "bind",
    "caller",
    "arguments",
];

impl FnLower<'_, '_> {
    /// Is `ty` a function type whose values may carry properties (a program with writes
    /// through `any`)?
    pub(super) fn fn_props(&self, ty: TyId) -> bool {
        self.cx.boxing.any_writes
            && matches!(self.cx.kind(ty), TyKind::FnPtr { .. } | TyKind::Closure(_))
    }

    /// The identity of the function value an `any`'s data pointer `data` stands for: its
    /// environment, or for one without (a declared function's value) the identity the runtime
    /// keeps for its code, null while it has none.
    fn fn_env(&mut self, data: vir::Local, ty: TyId) -> Operand {
        let vp = self.any_value_place(data, ty);
        let out = Place::local(self.temp(Ty::Ptr));
        let env = Operand::Copy(proj(&vp, Proj::Field(1)));
        self.assign(out.clone(), Rvalue::Use(env));
        let (none, join) = (self.new_block(), self.new_block());
        let nn = self.non_null(Operand::Copy(out.clone()));
        self.branch(nn, join, none);
        self.switch_to(none);
        let code = Operand::Copy(proj(&vp, Proj::Field(0)));
        self.call_rt(Rt::FnPropIdentityOf, vec![code], Some(out.clone()));
        self.goto(join);
        self.switch_to(join);
        Operand::Copy(out)
    }

    /// Give the function value at `fp` without an environment (a declared function's value)
    /// the identity the runtime keeps for its code, as its environment: every `any` holding the
    /// function then holds one function, which `===`, `Map` keys and properties see.
    pub(super) fn fn_identity_into(&mut self, fp: &Place) {
        let envp = proj(fp, Proj::Field(1));
        let env = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(envp.clone())));
        let (none, join) = (self.new_block(), self.new_block());
        let nn = self.non_null(env);
        self.branch(nn, join, none);
        self.switch_to(none);
        let code = Operand::Copy(proj(fp, Proj::Field(0)));
        self.call_rt(Rt::FnPropIdentity, vec![code], Some(envp));
        self.goto(join);
        self.switch_to(join);
    }

    /// Look property `key` up: the current block becomes the one where it is present, with the
    /// address of its `any` value; returns the block where it is not.
    fn fn_prop_find(&mut self, data: vir::Local, key: vir::Local, ty: TyId) -> (Operand, BlockId) {
        let env = self.fn_env(data, ty);
        let (has_env, found, absent) = (self.new_block(), self.new_block(), self.new_block());
        let nn = self.non_null(env.clone());
        self.branch(nn, has_env, absent);
        self.switch_to(has_env);
        let p = Place::local(self.temp(Ty::Ptr));
        let k = Operand::Copy(Place::local(key));
        self.call_rt(Rt::FnPropGet, vec![env, k], Some(p.clone()));
        let nn = self.non_null(Operand::Copy(p.clone()));
        self.branch(nn, found, absent);
        self.switch_to(found);
        (Operand::Copy(p), absent)
    }

    /// A share of the property value at address `p`.
    fn fn_prop_value(&mut self, p: Operand) -> Operand {
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let pp = self.operand_place(p, Ty::Ptr);
        self.share_value(Operand::Copy(proj(&pp, Proj::Deref(avt))), any)
    }

    /// The `AnyGet` slot body of a function type: its property `key`, else `undefined`.
    pub(super) fn fn_prop_get(&mut self, data: vir::Local, key: vir::Local, out: &Place, ty: TyId) {
        let (p, absent) = self.fn_prop_find(data, key, ty);
        let v = self.fn_prop_value(p);
        self.store(out.clone(), v);
        self.ret_status(ST_OK);
        self.switch_to(absent);
        for name in FN_PROTO.iter().chain(OBJECT_PROTO.iter()) {
            let next = self.on_key(key, name);
            self.ret_status(ST_UNSUPPORTED);
            self.switch_to(next);
        }
        self.any_nothing_into(Nothing::Undefined, out);
        self.ret_status(ST_OK);
    }

    /// The start of the `AnyCall` slot body of a function type: a property holding a function
    /// is called through its own apply slot (on a share that keeps it alive while it runs);
    /// without one the body goes on with the built-in members.
    pub(super) fn fn_prop_call(
        &mut self,
        (data, key, argv, argc, out): (vir::Local, vir::Local, vir::Local, vir::Local, vir::Local),
        ty: TyId,
    ) {
        let (p, absent) = self.fn_prop_find(data, key, ty);
        let v = self.fn_prop_value(p);
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let held = Place::local(self.copy_to_temp(v, avt));
        let (fd, fdesc) = self.any_parts(Operand::Copy(held.clone()), any);
        let a = [argv, argc, out].map(|l| Operand::Copy(Place::local(l)));
        let [av, ac, ao] = a;
        let p = vec![Ty::Ptr, Ty::Ptr, Ty::U64, Ty::Ptr];
        let st = self.any_slot(fdesc, SLOT_APPLY, vec![fd, av, ac, ao], p);
        self.drop_glue(held, any);
        self.terminate(Terminator::Return(st));
        self.switch_to(absent);
    }

    /// The `AnySet` slot body of a function type: the runtime keeps the value (which the slot
    /// owns) as property `key`.
    pub(super) fn fn_prop_set(
        &mut self,
        data: vir::Local,
        key: vir::Local,
        v: vir::Local,
        ty: TyId,
    ) {
        for name in ["name", "length"] {
            let next = self.on_key(key, name);
            self.any_set_refuse(v, ST_UNSUPPORTED);
            self.switch_to(next);
        }
        // A function without an environment (a declared function's value) gets the identity
        // the runtime keeps for its code, the same for every reference to the function; the
        // value this `any` holds keeps it as its environment.
        let vp = self.any_value_place(data, ty);
        self.fn_identity_into(&vp);
        let env = Operand::Copy(proj(&vp, Proj::Field(1)));
        let env = self.rvalue_temp(Ty::Ptr, Rvalue::Use(env));
        let (yes, no) = (self.new_block(), self.new_block());
        let nn = self.non_null(env.clone());
        self.branch(nn, yes, no);
        self.switch_to(yes);
        let (k, val) = (
            Operand::Copy(Place::local(key)),
            Operand::Copy(Place::local(v)),
        );
        self.call_rt(Rt::FnPropSet, vec![env, k, val], None);
        self.ret_status(ST_OK);
        // A generic closure's value without an environment: no identity to carry them yet.
        self.switch_to(no);
        self.any_set_refuse(v, ST_UNSUPPORTED);
    }

    /// The `AnyHas` slot body of a function type: its properties, and what every function and
    /// object has.
    pub(super) fn fn_prop_has(
        &mut self,
        data: vir::Local,
        key: vir::Local,
        ty: TyId,
        yes: i128,
        no: i128,
    ) {
        let (_, absent) = self.fn_prop_find(data, key, ty);
        self.ret_status(yes);
        self.switch_to(absent);
        self.has_names(key, super::any_has_builtin::FN_PROTO_NAMES, yes);
        self.has_names(key, &OBJECT_PROTO, yes);
        self.ret_status(no);
    }

    /// `delete f[key]` of a function: its property goes; `true` either way, as in Node.
    pub(super) fn fn_prop_delete(
        &mut self,
        data: vir::Local,
        key: vir::Local,
        ty: TyId,
        done: i128,
    ) {
        let env = self.fn_env(data, ty);
        let (yes, no) = (self.new_block(), self.new_block());
        let nn = self.non_null(env.clone());
        self.branch(nn, yes, no);
        self.switch_to(yes);
        let r = Place::local(self.temp(Ty::U8));
        let k = Operand::Copy(Place::local(key));
        self.call_rt(Rt::FnPropDelete, vec![env, k], Some(r));
        self.ret_status(done);
        self.switch_to(no);
        self.ret_status(done);
    }

    /// `Object.keys` of a function: its properties' names in insertion order, written to the
    /// `string[]` place `out`.
    pub(super) fn fn_prop_keys(&mut self, data: vir::Local, out: &Place, ty: TyId) {
        let str_ = self.cx.str_ty();
        let sa = self.cx.intern(TyKind::Array(str_));
        let v = self.array_lit(&[], sa);
        let svt = self.cx.ty(sa);
        let ap = Place::local(self.copy_to_temp(v, svt));
        let env = self.fn_env(data, ty);
        let (yes, done) = (self.new_block(), self.new_block());
        let nn = self.non_null(env.clone());
        self.branch(nn, yes, done);
        self.switch_to(yes);
        let n = Place::local(self.temp(Ty::U64));
        self.call_rt(Rt::FnPropLen, vec![env.clone()], Some(n.clone()));
        let arr = self.content(&ap, sa);
        let k = self.temp(Ty::U64);
        self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
        self.count_loop(k, Operand::Copy(n), |lw, kv| {
            let s = lw.temp(Ty::Agg(STR_AGG));
            let sp = lw.addr(Place::local(s));
            lw.call_rt(Rt::FnPropKey, vec![env.clone(), kv, sp], None);
            lw.push_value(&arr, str_, Operand::Copy(Place::local(s)));
        });
        self.goto(done);
        self.switch_to(done);
        self.store(out.clone(), Operand::Copy(ap));
    }
}
