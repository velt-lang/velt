//! Members of a value held as `any` that glue can call (any_glue.rs's slots): which method or
//! getter is ready to call from glue (`ready`), the receiver it gets (`any_this`, a share of the
//! object for an async method, whose promise keeps it), and the arguments converted from the
//! `any[]` a slot receives (`any_params`).

use velt_sema::hir::{DefId, DynMember, Nothing, PassMode, TyId, TyKind};

use super::{cint, FnLower};
use crate::vir::{self, BinOp, Callee, Operand, Place, Rvalue, Terminator, Ty};

/// A method or getter ready to call (only a `Proxy` method throws: proxy_throws.rs).
pub(super) struct Ready {
    pub(super) def: DefId,
    pub(super) targs: Vec<TyId>,
    pub(super) ret: TyId,
    pub(super) throws: Option<TyId>,
}

impl FnLower<'_, '_> {
    /// Member `m` of a value of type `ty`, when it can be called from glue.
    pub(super) fn ready(&mut self, ty: TyId, m: &DynMember) -> Option<Ready> {
        self.ready_with(ty, m, false)
    }

    /// [`ready`](Self::ready), also for a member that throws when `throwing` (a method called,
    /// a getter read or a setter written through `any`: its error leaves the slot as uncaught,
    /// `finish_ready_call`).
    pub(super) fn ready_with(&mut self, ty: TyId, m: &DynMember, throwing: bool) -> Option<Ready> {
        let (_, args) = self.plain_adt(ty)?;
        let owner = self.cx.subst(m.owner, &args);
        let TyKind::Adt(_, targs) = self.cx.kind(owner) else {
            return None;
        };
        let f = self.cx.fn_def(m.def);
        if f.generics as usize != targs.len() || f.is_generator {
            return None;
        }
        // An async method takes `this` owned (its promise keeps it): glue passes a share of a
        // class object (`invoke_ready`). Another owned `this` is not called from glue.
        let owned_class = f.is_async && self.cx.is_class(owner);
        if f.params
            .first()
            .is_none_or(|p| p.mode == PassMode::Owned && !owned_class)
        {
            return None;
        }
        let throws = self.cx.fn_throws(f, &targs).filter(|_| !f.is_async);
        (throws.is_none() || throwing || self.literal_accessor(owner, m) || self.is_proxy(owner))
            .then_some(())?;
        let (ret, _) = self.cx.call_sig(f);
        let ret = self.cx.subst(ret, &targs);
        if !self.cx.any_boxable(ret) {
            return None;
        }
        Some(Ready {
            def: m.def,
            targs,
            ret,
            throws,
        })
    }

    /// `this` for a method called on the value of type `ty` an `any`'s `data` holds.
    pub(super) fn any_this(&mut self, data: vir::Local, ty: TyId) -> Operand {
        let vp = self.any_value_place(data, ty);
        match self.cx.ty(ty) {
            Ty::Agg(_) => self.addr(vp),
            _ => Operand::Copy(vp),
        }
    }

    /// Call `r` with `this` and the owned argument values `args`; box the result into `out`.
    pub(super) fn any_invoke(
        &mut self,
        r: &Ready,
        this: Operand,
        args: Vec<(Place, TyId)>,
        out: &Place,
    ) {
        let std_result = self
            .cx
            .fn_def(r.def)
            .name
            .starts_with(velt_common::std_modules::CANONICAL_PREFIX);
        let v = self.invoke_ready(r, this, args);
        self.any_box_with(v, r.ret, out, std_result);
    }

    /// Call `r` with `this` and the owned argument values `args`: its result, owned (of type
    /// `r.ret`).
    pub(super) fn invoke_ready(
        &mut self,
        r: &Ready,
        this: Operand,
        args: Vec<(Place, TyId)>,
    ) -> Operand {
        let f = self.cx.fn_def(r.def);
        let modes: Vec<PassMode> = f.params.iter().map(|p| p.mode).collect();
        let owned_this = f
            .params
            .first()
            .filter(|p| f.is_async && p.mode == PassMode::Owned)
            .map(|p| p.ty);
        // Glue has no source location: a standard-library method it calls (a proxy's `__keys`)
        // reports none either, rather than its own line in the prelude.
        let fid = match self.panic_loc() {
            None if self.cx.tracks_caller(r.def) => self.cx.func(super::Work::Tracked(
                r.def,
                r.targs.clone(),
                super::srcloc::UNKNOWN_LOC,
            )),
            _ => self.cx.func_for(r.def, r.targs.clone()),
        };
        // An async method's owned `this` (`ready_with`): a share of the class object the `any`
        // holds, which the method's promise keeps.
        let this = match owned_this {
            Some(t) => {
                let t = self.cx.subst(t, &r.targs);
                self.share_value(this, t)
            }
            None => this,
        };
        let mut argv = vec![this];
        let mut after = vec![];
        for ((p, t), mode) in args.into_iter().zip(modes.into_iter().skip(1)) {
            match (self.cx.ty(t), mode) {
                (Ty::Unit, _) => {}
                (Ty::Agg(_), PassMode::Owned) => argv.push(self.addr(p)),
                (Ty::Agg(_), _) => {
                    argv.push(self.addr(p.clone()));
                    after.push((p, t));
                }
                (_, PassMode::Owned) => argv.push(Operand::Copy(p)),
                _ => {
                    argv.push(Operand::Copy(p.clone()));
                    after.push((p, t));
                }
            }
        }
        let v = self.finish_ready_call(Callee::Func(fid), argv, r);
        for (p, t) in after {
            self.drop_glue(p, t);
        }
        if let Operand::Copy(p) = &v {
            self.take_temp(&p.clone());
        }
        v
    }

    /// The arguments converted to `params` (owned temporaries): argument `k` checked as by
    /// `AnyTo`, a missing one `undefined` (`null` for a nullable parameter, a `TypeError` for
    /// another); extra arguments are dropped.
    pub(super) fn any_params(
        &mut self,
        argv: vir::Local,
        argc: vir::Local,
        params: &[TyId],
    ) -> Vec<(Place, TyId)> {
        let out = self.any_fixed_params(argv, argc, params);
        self.any_drop_args(argv, params.len() as u64, argc);
        out
    }

    /// [`Self::any_params`] without dropping the arguments past `params`.
    pub(super) fn any_fixed_params(
        &mut self,
        argv: vir::Local,
        argc: vir::Local,
        params: &[TyId],
    ) -> Vec<(Place, TyId)> {
        let any = self.cx.any_ty();
        let mut out = vec![];
        for (k, &p) in params.iter().enumerate() {
            let vt = self.cx.ty(p);
            // A `void` parameter (no argument passed): a stand-in place; a missing one is fine.
            let unit = vt == Ty::Unit;
            let slot = Place::local(self.temp(if unit { Ty::U8 } else { vt }));
            let has = self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(
                    BinOp::Lt,
                    cint(k as i128, Ty::U64),
                    Operand::Copy(Place::local(argc)),
                ),
            );
            let (yes, no, done) = (self.new_block(), self.new_block(), self.new_block());
            self.branch(has, yes, no);
            self.switch_to(yes);
            let ap = self.any_arg_place(argv, cint(k as i128, Ty::U64));
            let avt = self.cx.ty(any);
            let ap = Place::local(self.copy_to_temp(Operand::Copy(ap), avt));
            let v = self.any_unbox(&ap, any, p);
            if !unit {
                self.store(slot.clone(), v);
            }
            self.goto(done);
            self.switch_to(no);
            if unit {
            } else if self.cx.is_dynamic(p) {
                self.any_nothing_into(Nothing::Undefined, &slot);
            } else if matches!(self.cx.kind(p), TyKind::Option(..)) {
                let none = self.none_value(p);
                self.store(slot.clone(), none);
            } else {
                let want = self.any_target_name(p);
                let an = super::any_errors::article_of(&want);
                self.panic_msg(&format!(
                    "TypeError: the value is not {an} {want} (it is undefined)"
                ));
                self.terminate(Terminator::Unreachable);
                let dead = self.new_block();
                self.switch_to(dead);
            }
            self.goto(done);
            self.switch_to(done);
            out.push((slot, p));
        }
        out
    }
}
