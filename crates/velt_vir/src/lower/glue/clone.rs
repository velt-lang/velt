//! Clone glue bodies (`x.clone()`, owned copies of borrowed values): a bitwise copy first, then
//! every part that owns resources is replaced by its own deep copy. A resource-owning class with
//! a `clone()` of its own (`Cx::own_clone`, transfer.rs) is copied by that method. While a value
//! is transferred to another thread, a counted object referenced more than once is copied once
//! (`find_copy`, glue/transfer.rs), so the copy keeps the graph's sharing and cycles.

use velt_sema::hir::{PassMode, TyId, TyKind};

use super::{Glue, SLOT_CLONE};
use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cfunc, cint, unit, FnLower, Work};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    pub(super) fn clone_body(&mut self, src: vir::Local, dst: vir::Local, ty: TyId) {
        let s = self.deref_param(src, ty);
        let d = self.deref_param(dst, ty);
        let proxy = self.cx.proxiable(ty).then(|| {
            let done = self.new_block();
            let at = d.clone();
            self.proxy_glue_split(&s, done, |lw, up, pty| lw.clone_proxy(up, &at, pty));
            done
        });
        self.clone_expand(&s, &d, ty);
        if let Some(done) = proxy {
            self.goto(done);
            self.switch_to(done);
        }
        self.terminate(Terminator::Return(unit()));
    }

    /// A deep copy of the typed proxy whose `Proxy` object (untagged) is at `up`, stored
    /// tagged at `d`: a copy of the object, its target and its handler, as for an `any`
    /// holding it.
    fn clone_proxy(&mut self, up: Place, d: &Place, pty: TyId) {
        let out = Place::local(self.temp(Ty::Ptr));
        self.clone_into(up, out.clone(), pty);
        let t = self.proxy_tag(Operand::Copy(out));
        self.assign(d.clone(), Rvalue::Use(t));
    }

    fn clone_expand(&mut self, s: &Place, d: &Place, ty: TyId) {
        let promise = matches!(self.cx.kind(ty), TyKind::Promise(..));
        if !self.cx.is_class(ty) && !promise && self.cx.uncopyable_part(ty) == Some(ty) {
            // A disposable struct or union (classes: `obj_clone_body`).
            self.panic_cannot_copy("a", ty);
        }
        if self.cx.boxed(ty) {
            return self.clone_boxed(s, d, ty, |lw, sv, dv| lw.clone_inline(sv, dv, ty));
        }
        match self.cx.kind(ty) {
            TyKind::Str => self.str_share(s.clone(), d.clone()),
            TyKind::Adt(dd, _) if self.cx.is_class(ty) => {
                let obj = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(s.clone())));
                self.assign(d.clone(), Rvalue::Use(obj.clone()));
                let done = self.new_block();
                let nn = self.non_null(obj.clone());
                self.when(nn, done);
                let new = if self.cx.has_header(dd) {
                    let vt = self.obj_vtable(obj.clone(), ty);
                    let f = self.dispatch(vt, SLOT_CLONE);
                    self.call_entry(f, vec![obj], vec![Ty::Ptr], Ty::Ptr)
                } else {
                    self.call_glue(Glue::ObjClone, ty, vec![obj])
                };
                self.assign(d.clone(), Rvalue::Use(new));
                self.goto(done);
                self.switch_to(done);
            }
            TyKind::Adt(..) if self.is_enum(ty) => {
                self.assign(d.clone(), Rvalue::Use(Operand::Copy(s.clone())));
                let dd = d.clone();
                self.for_each_variant(s, ty, |lw, v, parts| {
                    let view = lw.cx.view(ty, v);
                    let dv = proj(&dd, Proj::Cast(view));
                    for (k, (sp, st)) in parts.into_iter().enumerate() {
                        lw.clone_into(sp, proj(&dv, Proj::Field(1 + k as u32)), st);
                    }
                });
            }
            TyKind::Adt(..) | TyKind::Tuple(_) => {
                let tys = self.cx.part_types(ty);
                for (i, t) in tys.into_iter().enumerate() {
                    if !self.cx.is_unit(t) {
                        let sp = self.field_place(s, ty, i as u32);
                        let dp = self.field_place(d, ty, i as u32);
                        self.clone_into(sp, dp, t);
                    }
                }
            }
            TyKind::Option(e, _) => self.clone_option(s, d, ty, e),
            TyKind::Array(e) => self.clone_array(s, d, e),
            TyKind::Shared(_) => {
                self.assign(d.clone(), Rvalue::Use(Operand::Copy(s.clone())));
                let ptr = Operand::Copy(s.clone());
                let done = self.new_block();
                let nn = self.non_null(ptr.clone());
                self.when(nn, done);
                self.call_rt(Rt::RcInc, vec![ptr], None);
                self.goto(done);
                self.switch_to(done);
            }
            TyKind::FnPtr { .. } | TyKind::Closure(_) => self.clone_closure(s, d),
            // Another holder of the same promise (shared promises, #212).
            TyKind::Promise(..) => {
                let f = self.share_promise(Operand::Copy(s.clone()), ty);
                self.assign(d.clone(), Rvalue::Use(f));
            }
            TyKind::Dyn(..) => self.clone_dyn(s, d),
            _ => self.assign(d.clone(), Rvalue::Use(Operand::Copy(s.clone()))),
        }
    }

    /// Deep copy of the inline value of a boxed array / object type.
    fn clone_inline(&mut self, s: &Place, d: &Place, ty: TyId) {
        if let TyKind::Array(e) = self.cx.kind(ty) {
            return self.clone_array(s, d, e);
        }
        self.assign(d.clone(), Rvalue::Use(Operand::Copy(s.clone())));
        let tys = self.cx.part_types(ty);
        for (i, t) in tys.into_iter().enumerate() {
            if !self.cx.is_unit(t) && self.cx.needs_drop(t) {
                let f = Proj::Field(self.cx.vir_field(ty, i as u32));
                self.clone_into(proj(s, f.clone()), proj(d, f), t);
            }
        }
    }

    fn clone_option(&mut self, s: &Place, d: &Place, ty: TyId, e: TyId) {
        if self.cx.ty(ty) == Ty::Ptr {
            self.clone_into(s.clone(), d.clone(), e);
            return;
        }
        self.assign(d.clone(), Rvalue::Use(Operand::Copy(s.clone())));
        let done = self.new_block();
        let some = self.option_is_some(s, ty);
        self.when(some, done);
        self.clone_into(proj(s, Proj::Field(1)), proj(d, Proj::Field(1)), e);
        self.goto(done);
        self.switch_to(done);
    }

    pub(in crate::lower) fn clone_array(&mut self, s: &Place, d: &Place, e: TyId) {
        // A view copies its mirror, brought up to date: a plain array (array_view.rs).
        let s = &match self.cx.array_view_of(e) {
            true => {
                let t = self.cx.intern(velt_sema::hir::TyKind::Array(e));
                self.view_inline(s.clone(), t, super::super::array_view::ViewSync::All)
            }
            false => s.clone(),
        };
        let len = self.rvalue_temp(Ty::U64, Rvalue::Use(Operand::Copy(proj(s, Proj::Field(1)))));
        let a = self.cx.array_agg();
        let empty = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, len.clone(), cint(0, Ty::U64)),
        );
        let (empty_bb, full_bb, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(empty, empty_bb, full_bb);
        self.switch_to(empty_bb);
        let zero = vec![cint(0, Ty::Ptr), cint(0, Ty::U64), cint(0, Ty::U64)];
        self.assign(d.clone(), Rvalue::Aggregate(a, zero));
        self.goto(done);
        self.switch_to(full_bb);
        let fresh = self.inline_array_with_len(len.clone(), e);
        let fp = Place::local(fresh);
        self.copy_elems(s, cint(0, Ty::U64), &fp, len, e, false);
        self.assign(d.clone(), Rvalue::Use(Operand::Copy(fp)));
        self.goto(done);
        self.switch_to(done);
    }

    fn clone_closure(&mut self, s: &Place, d: &Place) {
        let hdr = self.cx.closure_agg();
        self.assign(d.clone(), Rvalue::Use(Operand::Copy(s.clone())));
        let env = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(proj(s, Proj::Field(1)))));
        let done = self.new_block();
        let nn = self.non_null(env.clone());
        self.when(nn, done);
        let ep = self.operand_place(env.clone(), Ty::Ptr);
        let f = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(
                &proj(&ep, Proj::Deref(Ty::Agg(hdr))),
                Proj::Field(1),
            ))),
        );
        let has = self.non_null(f.clone());
        self.when(has, done);
        let new = self.call_entry(f, vec![env], vec![Ty::Ptr], Ty::Ptr);
        self.assign(proj(d, Proj::Field(1)), Rvalue::Use(new));
        self.goto(done);
        self.switch_to(done);
    }

    fn clone_dyn(&mut self, s: &Place, d: &Place) {
        self.assign(d.clone(), Rvalue::Use(Operand::Copy(s.clone())));
        if self.cx.has_proxies() {
            let (sd, dd) = (proj(s, Proj::Field(0)), proj(d, Proj::Field(0)));
            return self.proxy_dyn_data(
                &sd,
                |lw, up, pty| lw.clone_proxy(up, &dd, pty),
                |lw| lw.clone_dyn_data(s, d),
            );
        }
        self.clone_dyn_data(s, d);
    }

    /// The data of the interface value `s` (already copied bitwise into `d`), deep-copied by
    /// its table's `SLOT_CLONE` entry.
    fn clone_dyn_data(&mut self, s: &Place, d: &Place) {
        let vt = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(proj(s, Proj::Field(1)))));
        let done = self.new_block();
        let nn = self.non_null(vt.clone());
        self.when(nn, done);
        let f = self.dispatch(vt, SLOT_CLONE);
        let data = Operand::Copy(proj(s, Proj::Field(0)));
        let new = self.call_entry(f, vec![data], vec![Ty::Ptr], Ty::Ptr);
        self.assign(proj(d, Proj::Field(0)), Rvalue::Use(new));
        self.goto(done);
        self.switch_to(done);
    }

    /// Stop the program: a deep copy reached `ty`, which owns a resource it cannot duplicate.
    /// Sema rejects the copies it sees (`x.clone()`, a resource still used after `spawn`);
    /// this catches the rest (a generic `T`, the captures of a function value, the
    /// implementor behind an interface value), which would release the resource twice.
    /// `what` names the copied value ("a", or "a function value that captured a").
    pub(in crate::lower) fn panic_cannot_copy(&mut self, what: &str, ty: TyId) {
        let why = self.uncopyable_why(ty);
        let name = self.cx.type_name(ty);
        self.panic_msg(&format!("cannot copy {what} `{name}`: {why}"));
    }

    pub(super) fn obj_clone_body(&mut self, obj: vir::Local, ty: TyId) {
        if self.cx.is_generator_obj(ty) {
            // Its state (suspended locals, `finally` blocks to run) cannot be duplicated.
            self.panic_msg(
                "a generator cannot be copied (`clone()`, or a value passed to a spawned task)",
            );
            return;
        }
        if let Some(m) = self.cx.own_clone(ty) {
            // The class duplicates what it owns itself (a resource handle, #122).
            let this = Operand::Copy(Place::local(obj));
            let this = match self.cx.fn_def(m).params[0].mode {
                PassMode::Owned => self.share_value(this, ty),
                _ => this,
            };
            let targs = self.cx.method_targs(m, ty);
            let f = self.cx.func_for_direct(m, targs, self.mode);
            let out = self.temp(Ty::Ptr);
            // User code runs outside a transfer under way (velt_rt `transfer_map`): its own
            // copies are not looked up in the transfer's map.
            let depth = Place::local(self.temp(Ty::U32));
            self.call_rt(Rt::XferSuspend, vec![], Some(depth.clone()));
            self.call(
                vir::Callee::Func(f),
                vec![this],
                Some(Place::local(out)),
                false,
            );
            self.call_rt(Rt::XferResume, vec![Operand::Copy(depth)], None);
            self.terminate(Terminator::Return(Operand::Copy(Place::local(out))));
            return;
        }
        if self.cx.uncopyable_part(ty) == Some(ty) {
            self.panic_cannot_copy("a", ty);
        }
        // During a transfer, an object referenced more than once is copied once.
        let src = Operand::Copy(Place::local(obj));
        let found = (self.cx.counted(ty) && self.cx.copied_across(ty)).then(|| {
            self.find_copy(src.clone(), ty, |lw, copy| {
                lw.terminate(Terminator::Return(copy));
            })
        });
        let oa = self.cx.obj_agg(ty);
        let new = self.object_alloc(ty);
        if let Some(found) = found {
            self.record_copy(found, src, new.clone());
        }
        let src = proj(&Place::local(obj), Proj::Deref(Ty::Agg(oa)));
        let np = self.operand_place(new.clone(), Ty::Ptr);
        let dst = proj(&np, Proj::Deref(Ty::Agg(oa)));
        self.assign(dst, Rvalue::Use(Operand::Copy(src)));
        let tys = self.cx.adt_field_tys(ty);
        // A record's copy has its own enumerable string keys only, as JavaScript's structured
        // clone (`structuredClone`, a value sent to another task) makes it: not its symbol keys
        // or accessors (fields 2 and 3, std/record/symbols.vlt).
        let record = self.prelude_record(ty).is_some();
        for (i, t) in tys.into_iter().enumerate() {
            let dp = self.field_place(&np, ty, i as u32);
            if record && i >= 2 {
                let none = self.none_value(t);
                self.assign(dp, Rvalue::Use(none));
                continue;
            }
            let sp = self.field_place(&Place::local(obj), ty, i as u32);
            self.clone_into(sp, dp, t);
        }
        // A copy of a view is the view of the copy of its object (any_view.rs).
        if let TyKind::Adt(d, _) = self.cx.kind(ty) {
            self.proxy_review(new.clone(), d);
        }
        self.terminate(Terminator::Return(new));
    }

    pub(super) fn dyn_clone_body(&mut self, data: vir::Local, ty: TyId) {
        if self.cx.uncopyable(ty) {
            self.panic_cannot_copy("an interface value holding a", ty);
        }
        let out = self.temp(Ty::Ptr);
        if self.cx.is_class(ty) || self.cx.boxed(ty) {
            self.clone_into(Place::local(data), Place::local(out), ty);
        } else {
            let vt = self.cx.ty(ty);
            let b = self.alloc(vt);
            self.assign(Place::local(out), Rvalue::Use(b));
            let src = self.deref_param(data, ty);
            let dst = self.deref_param(out, ty);
            self.clone_into(src, dst, ty);
        }
        self.terminate(Terminator::Return(Operand::Copy(Place::local(out))));
    }
}

impl FnLower<'_, '_> {
    /// `velt_rt_fut_share(f, ...)`: another holder of promise `f` of type `ty` (#212), with the
    /// glue that shares and drops its result slot.
    pub(in crate::lower) fn share_promise(&mut self, f: Operand, ty: TyId) -> Operand {
        let slot = self.cx.promise_slot(ty);
        let vt = self.cx.ty(slot);
        let size = self.cx.size_align(vt).0;
        let share = if self.cx.needs_drop(slot) {
            cfunc(self.cx.func(Work::Glue(Glue::ShareSlot, slot)))
        } else {
            cint(0, Ty::Ptr)
        };
        let drop = self
            .result_drop_fn(slot)
            .unwrap_or_else(|| cint(0, Ty::Ptr));
        let out = self.temp(Ty::Ptr);
        let args = vec![f, cint(size as i128, Ty::U64), share, drop];
        self.call_rt(Rt::FutShare, args, Some(Place::local(out)));
        Operand::Copy(Place::local(out))
    }

    /// `Glue::ShareSlot`: another reference to the value at `src`, written to `dst`.
    pub(super) fn share_slot_body(&mut self, src: vir::Local, dst: vir::Local, ty: TyId) {
        self.cx.note_share(ty);
        let s = self.deref_param(src, ty);
        let d = self.deref_param(dst, ty);
        self.share_into(s, d, ty);
        self.terminate(Terminator::Return(unit()));
    }
}
