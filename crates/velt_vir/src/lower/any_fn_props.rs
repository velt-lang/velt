//! Properties of function values through `any` (#924; rt_abi.md "Function properties"): as in
//! JavaScript, a function is an object a program may add properties to (`factory.__setup = setup`).
//! The member slots of a function type's `any` table (any_glue.rs, any_set.rs, any_has.rs,
//! any_keys.rs, any_delete.rs) look a key up in the runtime's table of the function's properties
//! (std/lang/hooks/fn_props.velt, through its hooks), keyed by its closure environment, which is
//! the function's identity: a program that writes through `any` and holds functions there gives
//! every closure a counted environment (`Boxing::fn_identity`), so copies of the function share it,
//! and its properties; a function value without one (a declared function's) has the identity std
//! keeps for its code, made at the first property write to any reference to it.
//!
//! A property read or call that finds nothing goes on as before: `name` and `length` are the
//! function's own (fn_shapes.rs), the other built-in members of functions (`call`, ...) are not
//! supported yet, and another key is `undefined`, as in Node. Writing `name` or `length` (read-only
//! in JavaScript) is not supported either. In a program without writes through `any` no function
//! has properties, and the slots are the ones they were before.

use velt_sema::hir::{Nothing, TyId, TyKind};

use super::any_glue::OBJECT_PROTO;
use super::any_has_builtin::{FN_OWN_UNKNOWN, FN_PROTO_NAMES};
use super::any_member::{SLOT_APPLY, ST_OK, ST_UNSUPPORTED};
use super::hook_calls::{self as hooks, ty, Construct, HookTy};
use super::operand::proj;
use super::{cint, FnLower, ScopeKind};
use crate::vir::{self, BlockId, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    /// Is `ty` a function type whose values may carry properties (a program with writes
    /// through `any`)?
    pub(super) fn fn_props(&self, ty: TyId) -> bool {
        self.cx.boxing.any_writes
            && matches!(self.cx.kind(ty), TyKind::FnPtr { .. } | TyKind::Closure(_))
    }

    /// `f` in a scope of its own, which a hook call needs (glue bodies may have none); the
    /// call's result is the caller's (`call_std_hook`).
    fn fn_props_scoped<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        self.push_scope(ScopeKind::Block);
        let r = f(self);
        self.pop_scope();
        r
    }

    /// An environment (or code) pointer as the hooks' `u64`.
    fn fn_props_word(&mut self, p: Operand) -> Operand {
        self.rvalue_temp(Ty::U64, Rvalue::Cast(p, Ty::U64))
    }

    /// The program's normal end (`velt_main`'s, after module state went): std's table releases
    /// the identities of declared functions for the leak counters (fn_props_env.velt
    /// `releaseAtExit`), in a program that may have made one and has the table checked. Not on a
    /// JS host, which has no leak counters (nor the C library that asks for them).
    pub(super) fn fn_props_exit(&mut self) {
        let name = velt_common::std_hook!(fn_props_exit);
        let live = (self.cx.prelude_def(name)).is_some_and(|d| !self.cx.hir.unchecked.contains(&d));
        let js = super::host::js_host(&self.cx.target);
        if live && !js && (self.cx.boxing.any_writes || self.cx.fn_identity()) {
            self.fn_props_scoped(|lw| {
                hooks::fn_props_exit(lw, Construct::FnProps).emit(lw);
            });
        }
    }

    /// Before std's table of a function's properties is reached: a declared function's
    /// are module state, so the module lock comes first, as for an access of module state
    /// (std/lang/hooks/sched_module.velt `touch`; the table's own lock is taken after it). Which
    /// function it is is known only at run time, so any function's properties take it. On
    /// WebAssembly, one thread, nothing.
    fn fn_props_touch(&mut self) {
        if self.std_sched() {
            self.module_lock(super::module_state::Access::Touch, vec![], None);
        }
    }

    /// The identity of the function value an `any`'s data pointer `data` stands for: its
    /// environment, or for one without (a declared function's value) the identity std keeps for
    /// its code, null while it has none.
    fn fn_env(&mut self, data: vir::Local, ty: TyId) -> Operand {
        let vp = self.any_value_place(data, ty);
        // A wrapper's properties are those of the function it wraps (closure/fn_wrappers.rs).
        let vp = self.fn_root(&vp);
        let out = Place::local(self.temp(Ty::Ptr));
        let env = Operand::Copy(proj(&vp, Proj::Field(1)));
        self.assign(out.clone(), Rvalue::Use(env));
        let (none, join) = (self.new_block(), self.new_block());
        let nn = self.non_null(Operand::Copy(out.clone()));
        self.branch(nn, join, none);
        self.switch_to(none);
        let code = Operand::Copy(proj(&vp, Proj::Field(0)));
        let code = ty::U64::arg(self.fn_props_word(code));
        let id = self.fn_props_scoped(|lw| {
            hooks::fn_prop_identity_of(lw, Construct::FnProps, code)
                .emit(lw)
                .op()
        });
        self.assign(out.clone(), Rvalue::Cast(id, Ty::Ptr));
        self.goto(join);
        self.switch_to(join);
        Operand::Copy(out)
    }

    /// Give the function value at `fp` without an environment (a declared function's value)
    /// the identity std keeps for its code, as its environment: every `any` holding the
    /// function then holds one function, which `===`, `Map` keys and properties see.
    pub(super) fn fn_identity_into(&mut self, fp: &Place) {
        let envp = proj(fp, Proj::Field(1));
        let env = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(envp.clone())));
        let (none, join) = (self.new_block(), self.new_block());
        let nn = self.non_null(env);
        self.branch(nn, join, none);
        self.switch_to(none);
        let code = Operand::Copy(proj(fp, Proj::Field(0)));
        let code = ty::U64::arg(self.fn_props_word(code));
        let id = self.fn_props_scoped(|lw| {
            hooks::fn_prop_identity(lw, Construct::FnProps, code)
                .emit(lw)
                .op()
        });
        self.assign(envp, Rvalue::Cast(id, Ty::Ptr));
        self.goto(join);
        self.switch_to(join);
    }

    /// Look property `key` up: the current block becomes the one where it is present, with the
    /// function's environment (as a `u64`) and the property's index; returns the block where it
    /// is not.
    fn fn_prop_find(
        &mut self,
        data: vir::Local,
        key: vir::Local,
        ty: TyId,
    ) -> ((Operand, Operand), BlockId) {
        let env = self.fn_env(data, ty);
        let (has_env, found, absent) = (self.new_block(), self.new_block(), self.new_block());
        let nn = self.non_null(env.clone());
        self.branch(nn, has_env, absent);
        self.switch_to(has_env);
        let env = self.fn_props_word(env);
        let k = ty::Str::arg(Operand::Copy(Place::local(key)));
        self.fn_props_touch();
        let e = ty::U64::arg(env.clone());
        let i = self.fn_props_scoped(|lw| {
            hooks::fn_prop_index(lw, Construct::FnProps, e, k)
                .emit(lw)
                .op()
        });
        let i = self.rvalue_temp(Ty::I64, Rvalue::Use(i));
        let none = cint(-1, Ty::I64);
        let found_it = self.rvalue_temp(Ty::Bool, Rvalue::Binary(vir::BinOp::Ne, i.clone(), none));
        self.branch(found_it, found, absent);
        self.switch_to(found);
        ((env, i), absent)
    }

    /// A share of property `i` of the function with environment `env` (`fn_prop_find`'s), owned.
    fn fn_prop_value(&mut self, (env, i): (Operand, Operand)) -> Operand {
        let (e, i) = (ty::U64::arg(env), ty::I64::arg(i));
        self.fn_props_scoped(|lw| hooks::fn_prop_at(lw, Construct::FnProps, e, i).emit_std(lw))
    }

    /// The `AnyGet` slot body of a function type: its property `key`, else `undefined`.
    pub(super) fn fn_prop_get(&mut self, data: vir::Local, key: vir::Local, out: &Place, ty: TyId) {
        let (p, absent) = self.fn_prop_find(data, key, ty);
        let v = self.fn_prop_value(p);
        self.store(out.clone(), v);
        self.ret_status(ST_OK);
        self.switch_to(absent);
        self.fn_own_get(data, key, out, ty);
        // What every function has from JavaScript itself or `Function.prototype`, not
        // supported through `any` yet.
        self.has_names(key, FN_PROTO_NAMES, ST_UNSUPPORTED);
        self.has_names(key, FN_OWN_UNKNOWN, ST_UNSUPPORTED);
        self.has_names(key, &OBJECT_PROTO, ST_UNSUPPORTED);
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

    /// The `AnySet` slot body of a function type: std's table keeps the value (which the slot
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
        // std keeps for its code, the same for every reference to the function; the
        // value this `any` holds keeps it as its environment.
        let vp = self.any_value_place(data, ty);
        self.fn_identity_into(&vp);
        // A wrapper's properties are those of the function it wraps (closure/fn_wrappers.rs).
        let vp = match self.cx.boxing.fn_wrappers {
            true => {
                let root = self.fn_root(&vp);
                self.fn_identity_into(&root);
                root
            }
            false => vp,
        };
        let env = Operand::Copy(proj(&vp, Proj::Field(1)));
        let env = self.rvalue_temp(Ty::Ptr, Rvalue::Use(env));
        let (yes, no) = (self.new_block(), self.new_block());
        let nn = self.non_null(env.clone());
        self.branch(nn, yes, no);
        self.switch_to(yes);
        let env = ty::U64::arg(self.fn_props_word(env));
        let (k, val) = (
            ty::Str::arg(Operand::Copy(Place::local(key))),
            ty::Any::moved(Operand::Copy(Place::local(v))),
        );
        self.fn_props_touch();
        self.fn_props_scoped(|lw| {
            hooks::fn_prop_set(lw, Construct::FnProps, env, k, val).emit(lw);
        });
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
        self.has_names(key, FN_PROTO_NAMES, yes);
        // A declared function's own `prototype` or an arrow's none: not known here.
        self.has_names(key, FN_OWN_UNKNOWN, super::any_has::HAS_UNSUPPORTED);
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
        let env = ty::U64::arg(self.fn_props_word(env));
        let k = ty::Str::arg(Operand::Copy(Place::local(key)));
        self.fn_props_touch();
        self.fn_props_scoped(|lw| {
            hooks::fn_prop_delete(lw, Construct::FnProps, env, k).emit(lw);
        });
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
        let env = self.fn_props_word(env);
        self.fn_props_touch();
        let e = ty::U64::arg(env.clone());
        let n =
            self.fn_props_scoped(|lw| hooks::fn_prop_len(lw, Construct::FnProps, e).emit(lw).op());
        let n = self.rvalue_temp(Ty::U64, Rvalue::Use(n));
        let arr = self.content(&ap, sa);
        let k = self.temp(Ty::U64);
        self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
        self.count_loop(k, n, |lw, kv| {
            let (e, i) = (ty::U64::arg(env.clone()), ty::U64::arg(kv));
            let s = lw.fn_props_scoped(|lw| {
                hooks::fn_prop_key(lw, Construct::FnProps, e, i).emit_std(lw)
            });
            lw.push_value(&arr, str_, s);
        });
        self.goto(done);
        self.switch_to(done);
        self.store(out.clone(), Operand::Copy(ap));
    }
}
