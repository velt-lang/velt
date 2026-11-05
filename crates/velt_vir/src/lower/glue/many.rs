//! Many-threads check glue (`ManyCheck`, lower/transfer.rs `many_check`): a value that becomes
//! reachable from several threads at once — put in `shared(x)`, or captured by an HTTP
//! handler, whose requests run concurrently — must not hold a function value that captured a
//! resource without `clone()`. Each call of such a function shares the capture with the call
//! (async_fn/ctor.rs `take_capture`), and calls from several threads would update its count
//! concurrently. The glue walks the value and panics on one; values that cannot reach a
//! function or interface value are not walked.
//!
//! Function values and interface values are checked through the entries they carry: an
//! environment's transfer entry and the `SLOT_TRANSFER` vtable entry, called with the pointer
//! tagged (low bit set, [`FnLower::tagged`]): in that mode the entry checks instead of
//! transferring and returns the pointer untagged ([`FnLower::check_if_tagged`]).
//!
//! The strict walk, for a function in `shared(...)`, also panics on what a call's copy of the
//! captures would lose: a closure that changes or shares what it captured, a lock or an `Atomic`
//! held by value, and a class whose `clone()` may change its object, which every call would run on
//! one object at once. A `Mutex` around the function needs none of that: the calls run one at a
//! time. It is generated apart (`Glue::ManyCheckStrict`, [`FnLower::many_strict`]), and an entry
//! knows it by the pointer's other tag, in a program with a function in `shared(...)` only.
//!
//! The strict walk goes through an object once, so a cycle ends: it is a transfer scope with nothing
//! copied (velt_rt `transfer_map`), each object it reaches recorded as its own copy. It runs no user
//! code and is never inside a transfer (the glue that transfers runs user code only with the
//! transfer suspended, and a caught panic forgets what it left open), so its scope has only its own
//! objects, and its `end` only forgets them. It ends the scope before it panics. The plain walk runs
//! at every `Mutex.with` and has no scope.

use velt_sema::hir::{TyId, TyKind};

use super::{Glue, SLOT_TRANSFER};
use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cint, unit, FnLower, Work};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// The other tag of a pointer given to an entry for the strict walk. Objects are aligned to 8.
const STRICT_TAG: i128 = 4;

impl FnLower<'_, '_> {
    pub(super) fn many_check_body(&mut self, p: vir::Local, ty: TyId) {
        let place = self.deref_param(p, ty);
        if self.cx.proxiable(ty) {
            // A typed proxy: its `Proxy` object is checked, as an `any` holding it would be.
            let done = self.new_block();
            self.proxy_glue_split(&place, done, |lw, up, pty| lw.many_check(up, pty));
            self.many_check_expand(&place, ty);
            self.goto(done);
            self.switch_to(done);
            self.terminate(Terminator::Return(unit()));
            return;
        }
        self.many_check_expand(&place, ty);
        self.terminate(Terminator::Return(unit()));
    }

    /// Stop the program with `msg` from inside a walk (module docs).
    pub(super) fn walk_panic(&mut self, msg: &str) {
        self.end_walk_scope();
        self.panic_msg(msg);
    }

    /// The strict walk's scope ends (module docs).
    pub(super) fn end_walk_scope(&mut self) {
        if self.many_strict {
            self.call_rt(Rt::XferEnd, vec![], None);
        }
    }

    /// In the strict walk: panic with `msg`.
    pub(super) fn panic_if_strict(&mut self, msg: &str) {
        if self.many_strict {
            self.walk_panic(msg);
        }
    }

    /// Continue only the first time this walk reaches the object at `p`; otherwise go to `done`.
    fn walk_first(&mut self, p: Operand, done: vir::BlockId) {
        if !self.many_strict {
            return;
        }
        let found = self.temp(Ty::Ptr);
        self.call_rt(Rt::XferFind, vec![p.clone()], Some(Place::local(found)));
        let bits = self.cast_to(Operand::Copy(Place::local(found)), Ty::Ptr, Ty::U64);
        // Null: not reached yet. 1: outside a scope, nothing to remember.
        let first = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Le, bits, cint(1, Ty::U64)));
        self.when(first, done);
        self.call_rt(Rt::XferRecord, vec![p.clone(), p], None);
    }

    /// The panic of the strict walk for a value of `ty` itself, a lock or an `Atomic`, or a resource
    /// that isn't a class.
    fn panic_if_strict_held(&mut self, ty: TyId) {
        if !self.many_strict {
            return;
        }
        let name = self.cx.type_name(ty);
        match self.cx.kind(ty) {
            TyKind::Adt(d, _) if crate::lower::atomic::is_atomic(self.cx, d) => self.walk_panic(&format!(
                "a function that captured an `{name}` by value is in `shared(...)`: each call would change an `Atomic` of its own, so the change would be lost; keep the value in a `shared(...)`"
            )),
            _ if self.cx.is_lock(ty) => self.walk_panic(&format!(
                "a function that captured a `{name}` by value is in `shared(...)`: each call would have a lock of its own; share the lock: `shared(new {name}(…))`"
            )),
            _ if !self.cx.is_class(ty) => self.panic_if_uncopyable(ty),
            _ => {}
        }
    }

    /// In the strict walk, a resource no call's copy can be made of: panic where the function is put
    /// in `shared(...)`, not at every call.
    fn panic_if_uncopyable(&mut self, ty: TyId) {
        if self.cx.uncopyable(ty) {
            let why = self.uncopyable_why(ty);
            let name = self.cx.type_name(ty);
            self.walk_panic(&format!(
                "a function that captured a `{name}` is in `shared(...)`: each call runs on a copy of what it captured, and {why}"
            ));
        }
    }

    /// The panics of the strict walk for the object at `obj` of class `ty` (module docs).
    pub(super) fn panic_if_strict_object(&mut self, obj: &Place, ty: TyId) {
        if !self.many_strict {
            return;
        }
        let name = self.cx.type_name(ty);
        if self.cx.clone_may_change(ty) {
            self.walk_panic(&format!(
                "a function that captured a `{name}` is in `shared(...)`: calls from several threads would run its `clone()` on the same object at once; put the function in a `Mutex`"
            ));
        }
        self.panic_if_uncopyable(ty);
        if self.cx.is_regexp(ty) {
            self.panic_if_regexp_moves(obj, ty);
        }
    }

    /// A `g` or `y` regex changes its `lastIndex` as it matches, which a call's copy would lose.
    fn panic_if_regexp_moves(&mut self, obj: &Place, ty: TyId) {
        for flag in ["global", "sticky"] {
            let Some(i) = self.cx.field_index(ty, flag) else {
                continue;
            };
            let set = Operand::Copy(self.field_place(obj, ty, i));
            let done = self.new_block();
            self.when(set, done);
            self.walk_panic(
                "a function that captured a `g` or `y` `RegExp` is in `shared(...)`: each call runs on a copy of what it captured, so the change to its `lastIndex` would be lost; make the regex inside the function, or put the function in a `Mutex`",
            );
            self.goto(done);
            self.switch_to(done);
        }
    }

    fn many_check_expand(&mut self, place: &Place, ty: TyId) {
        self.panic_if_strict_held(ty);
        if self.cx.boxed(ty) {
            return self.many_check_boxed(place, ty);
        }
        match self.cx.kind(ty) {
            TyKind::Adt(d, _) if self.cx.is_class(ty) => {
                let obj = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(place.clone())));
                let done = self.new_block();
                let nn = self.non_null(obj.clone());
                self.when(nn, done);
                self.walk_first(obj.clone(), done);
                let tagged = self.tagged(obj.clone());
                if self.cx.has_header(d) {
                    let vt = self.obj_vtable(obj, ty);
                    let f = self.dispatch_class_glue(vt, SLOT_TRANSFER, ty);
                    self.call_entry(f, vec![tagged], vec![Ty::Ptr], Ty::Ptr);
                } else {
                    self.call_glue(Glue::ObjTransfer, ty, vec![tagged]);
                }
                self.goto(done);
                self.switch_to(done);
            }
            TyKind::Adt(..) if self.is_enum(ty) => self.many_check_variants(place, ty),
            TyKind::Result(..) => self.many_check_variants(place, ty),
            TyKind::Adt(..) | TyKind::Tuple(_) => {
                for (i, t) in self.cx.part_types(ty).into_iter().enumerate() {
                    if !self.cx.is_unit(t) {
                        let fp = self.field_place(place, ty, i as u32);
                        self.many_check(fp, t);
                    }
                }
            }
            TyKind::Option(e, _) => self.many_check_option(place, ty, e),
            TyKind::Array(e) => self.many_check_elems(place, e),
            TyKind::FnPtr { .. } | TyKind::Closure(_) => self.many_check_closure(place),
            TyKind::Dyn(..) if self.cx.has_proxies() => {
                let data = proj(place, Proj::Field(0));
                self.proxy_dyn_data(
                    &data,
                    |lw, up, pty| lw.many_check(up, pty),
                    |lw| lw.many_check_dyn(place),
                );
            }
            TyKind::Dyn(..) => self.many_check_dyn(place),
            _ => {}
        }
    }

    /// The interface value at `place`, checked through its table's `SLOT_TRANSFER` entry.
    fn many_check_dyn(&mut self, place: &Place) {
        let vt = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(place, Proj::Field(1)))),
        );
        let done = self.new_block();
        let nn = self.non_null(vt.clone());
        self.when(nn, done);
        let data = Operand::Copy(proj(place, Proj::Field(0)));
        let tagged = self.tagged(data);
        let f = self.dispatch(vt, SLOT_TRANSFER);
        self.call_entry(f, vec![tagged], vec![Ty::Ptr], Ty::Ptr);
        self.goto(done);
        self.switch_to(done);
    }

    fn many_check_boxed(&mut self, place: &Place, ty: TyId) {
        let payload = self.cx.payload_ty(ty);
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(place.clone())));
        let done = self.new_block();
        let nn = self.non_null(p.clone());
        self.when(nn, done);
        let pp = self.operand_place(p, Ty::Ptr);
        let value = proj(&pp, Proj::Deref(payload));
        match self.cx.kind(ty) {
            TyKind::Array(e) => self.many_check_elems(&value, e),
            _ => {
                for (i, t) in self.cx.part_types(ty).into_iter().enumerate() {
                    if !self.cx.is_unit(t) {
                        let f = Proj::Field(self.cx.vir_field(ty, i as u32));
                        self.many_check(proj(&value, f), t);
                    }
                }
            }
        }
        self.goto(done);
        self.switch_to(done);
    }

    fn many_check_variants(&mut self, place: &Place, ty: TyId) {
        self.for_each_variant(place, ty, |lw, _, parts| {
            for (pp, pt) in parts {
                lw.many_check(pp, pt);
            }
        });
    }

    fn many_check_option(&mut self, place: &Place, ty: TyId, e: TyId) {
        if self.cx.ty(ty) == Ty::Ptr {
            return self.many_check(place.clone(), e);
        }
        let done = self.new_block();
        let some = self.option_is_some(place, ty);
        self.when(some, done);
        self.many_check(proj(place, Proj::Field(1)), e);
        self.goto(done);
        self.switch_to(done);
    }

    fn many_check_elems(&mut self, arr: &Place, e: TyId) {
        if !self.many_reaches(e) {
            return;
        }
        let k = self.temp(Ty::U64);
        self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
        let len = Operand::Copy(proj(arr, Proj::Field(1)));
        self.count_loop(k, len, |lw, k| {
            let p = lw.elem_place(arr, k, e);
            lw.many_check(p, e);
        });
    }

    /// `{ code, env }`: a heap env is checked by its transfer entry (`build_env_transfer`).
    fn many_check_closure(&mut self, place: &Place) {
        let hdr = self.cx.closure_agg();
        let env = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(place, Proj::Field(1)))),
        );
        let done = self.new_block();
        let nn = self.non_null(env.clone());
        self.when(nn, done);
        let ep = self.operand_place(env.clone(), Ty::Ptr);
        let header = proj(&ep, Proj::Deref(Ty::Agg(hdr)));
        let drop = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&header, Proj::Field(0)))),
        );
        let heap = self.non_null(drop);
        self.when(heap, done);
        let f = self.env_transfer_entry(env.clone());
        // A frame env that owns captures has a drop entry and no transfer entry.
        let has = self.non_null(f.clone());
        self.when(has, done);
        self.walk_first(env.clone(), done);
        let tagged = self.tagged(env);
        self.call_entry(f, vec![tagged], vec![Ty::Ptr], Ty::Ptr);
        self.goto(done);
        self.switch_to(done);
    }

    /// `p` with its low bit set: an entry called with it checks instead of transferring. In the
    /// strict walk, with `STRICT_TAG` as well.
    fn tagged(&mut self, p: Operand) -> Operand {
        let tag = match self.many_strict {
            true => 1 + STRICT_TAG,
            false => 1,
        };
        self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Binary(BinOp::PtrAdd, p, cint(tag, Ty::I64)),
        )
    }

    /// Prologue of an entry taking a possibly tagged pointer `p` (module docs): when tagged,
    /// run `check` on the untagged pointer and return that; otherwise continue. `check` is run
    /// twice in a program with a function in `shared(...)`, for each walk.
    pub(super) fn check_if_tagged(&mut self, p: vir::Local, check: impl Fn(&mut Self, Operand)) {
        let bits = self.rvalue_temp(
            Ty::U64,
            Rvalue::Cast(Operand::Copy(Place::local(p)), Ty::U64),
        );
        let low = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::BitAnd, bits.clone(), cint(1, Ty::U64)),
        );
        let is = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, low, cint(0, Ty::U64)));
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(is, yes, no);
        self.switch_to(yes);
        if self.cx.boxing.shared_fns {
            let tag = self.rvalue_temp(
                Ty::U64,
                Rvalue::Binary(BinOp::BitAnd, bits, cint(STRICT_TAG, Ty::U64)),
            );
            let is = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, tag, cint(0, Ty::U64)));
            let (strict, plain) = (self.new_block(), self.new_block());
            self.branch(is, strict, plain);
            self.switch_to(strict);
            self.many_strict = true;
            self.run_check(p, 1 + STRICT_TAG, &check);
            self.many_strict = false;
            self.switch_to(plain);
        }
        self.run_check(p, 1, &check);
        self.switch_to(no);
    }

    /// `check` on `p` without its tag `tag`, and return that.
    fn run_check(&mut self, p: vir::Local, tag: i128, check: &impl Fn(&mut Self, Operand)) {
        let untagged = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Binary(
                BinOp::PtrAdd,
                Operand::Copy(Place::local(p)),
                cint(-tag, Ty::I64),
            ),
        );
        check(self, untagged.clone());
        self.terminate(Terminator::Return(untagged));
    }

    /// One walk of the value at `p` (concrete type `ty`), which every thread can now reach;
    /// `strict` for a function in `shared(...)` (module docs).
    pub(in crate::lower) fn many_walk(&mut self, strict: bool, p: Place, ty: TyId) {
        if !self.cx.reaches_fn(ty) {
            return;
        }
        self.many_strict = strict;
        if strict {
            self.cx.note_shared_fn();
            self.call_rt(Rt::XferBegin, vec![], None);
        }
        self.many_check(p, ty);
        self.end_walk_scope();
        self.many_strict = false;
    }

    /// Can the walk being generated find anything in a value of `t`?
    fn many_reaches(&mut self, t: TyId) -> bool {
        match self.many_strict {
            true => self.cx.reaches_held(t),
            false => self.cx.reaches_fn(t),
        }
    }

    /// Check the value at `p` (concrete type `ty`) with the walk being generated.
    pub(in crate::lower) fn many_check(&mut self, p: Place, ty: TyId) {
        if !self.many_reaches(ty) {
            return;
        }
        let a = self.addr(p);
        let glue = match self.many_strict {
            true => Glue::ManyCheckStrict,
            false => Glue::ManyCheck,
        };
        let f = self.cx.func_direct(Work::Glue(glue, ty), self.mode);
        self.call(vir::Callee::Func(f), vec![a], None, false);
    }
}
