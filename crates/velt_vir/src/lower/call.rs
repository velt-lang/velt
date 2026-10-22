//! Calls to user functions and `declare function` externs: argument passing per `PassMode`,
//! aggregate results through a caller-provided out-pointer, and `Result` checks after calls to
//! throwing functions (convention in lib.rs). Calls through function values, vtables and
//! interface values use the borrow ABI (callee.rs).

use velt_sema::hir::{self, DefId, PassMode, TyId, TyKind, UseMode};

use super::operand::proj;
use super::{ice, unit, FnLower, LkMode, Work};
use crate::vir::{self, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    pub(super) fn call_expr(
        &mut self,
        callee: &hir::Callee,
        args: &[hir::Expr],
        ty: TyId,
    ) -> Operand {
        let transfer = std::mem::take(&mut self.transfer_call);
        match callee {
            hir::Callee::Intrinsic(i) => self.intrinsic(*i, args, ty),
            hir::Callee::Def(def, targs) => match self.cx.hir.def(*def) {
                hir::Def::Fn(_) => {
                    let targs: Vec<TyId> = targs.iter().map(|&t| self.sub(t)).collect();
                    let v = self.call_def(*def, targs, vec![], args);
                    self.after_super_inits(*def);
                    v
                }
                hir::Def::ExternFn(_) => self.extern_call(*def, args),
                _ => ice("call of a non-function definition"),
            },
            hir::Callee::Indirect(f) => self.call_indirect(f, args, ty, transfer),
            hir::Callee::Virtual { slot } => self.call_virtual(*slot, args, ty, transfer),
            hir::Callee::Dyn { slot } => self.call_dyn(*slot, args, ty, transfer),
            hir::Callee::ParamMethod {
                iface,
                iface_args,
                slot,
                method_type_args,
            } => {
                let recv = args
                    .first()
                    .unwrap_or_else(|| ice("method call without receiver"));
                let recv_ty = self.sub(recv.ty);
                let iargs: Vec<TyId> = iface_args.iter().map(|&t| self.sub(t)).collect();
                let (def, mut targs) = self.resolve_param_method(*iface, &iargs, *slot, recv_ty);
                // A generic method's own type args follow its owner's.
                targs.extend(method_type_args.iter().map(|&t| self.sub(t)));
                self.call_def(def, targs, vec![], args)
            }
        }
    }

    /// Direct call of a function instance; `pre` are already-lowered leading arguments.
    pub(super) fn call_def(
        &mut self,
        def: DefId,
        targs: Vec<TyId>,
        pre: Vec<Operand>,
        args: &[hir::Expr],
    ) -> Operand {
        let f = self.cx.fn_def(def);
        let modes: Vec<PassMode> = f.params[pre.len()..].iter().map(|p| p.mode).collect();
        let (ret, throws) = self.cx.call_sig(f);
        // Frozen counted arguments go to an instance that neither changes nor counts them.
        let frozen = self.frozen_mask(&modes, args, pre.len());
        let at = self.panic_loc().filter(|_| self.cx.tracks_caller(def));
        // A direct call: the callee runs in this function's mode (`Work::Checked`).
        let work = match (frozen, at) {
            (0, Some(at)) => Work::Tracked(def, targs.clone(), at),
            (0, None) => Work::Fn(def, targs.clone()),
            (mask, at) => {
                let fns = self.known_fns_of(&modes, args, pre.len());
                Work::Frozen(def, targs.clone(), mask, at, fns)
            }
        };
        // A method of what a `read` callback captured (the caller's `push`, `set`, `add`), or a
        // function changing it, runs by the counts outside read mode, freeing after the reads
        // (`LkMode::Deferred`): what it is given to keep is counted the same way, or a copy of
        // what may be the store's (caller_stores.rs `keep_for_caller`), and its result is handed
        // to the reader (`hand_to_reader`). A result read mode could not take over (an object
        // outside the lock set holding lock-set objects, as an iterator) leaves the call in
        // read mode.
        let changes_captured = pre.is_empty()
            && args.iter().zip(&modes).enumerate().any(|(i, (a, m))| {
                (i == 0 && f.self_ty.is_some() || *m == PassMode::BorrowMut)
                    && self.captured_root(a)
            });
        let rty = self.cx.subst(ret, &targs);
        let plain = changes_captured && (!self.needs_drop(rty) || self.hand_ok(rty));
        let mode = match plain {
            true => LkMode::Deferred,
            false => self.mode,
        };
        let fid = self.cx.func_direct(work, mode);
        // Not `super.m()`: JavaScript calls the base method without reading it from `this`.
        let sup = std::mem::take(&mut self.super_call);
        if f.self_ty.is_some() && !args.is_empty() && pre.is_empty() && !sup {
            let rty = self.sub(args[0].ty);
            if self.cx.is_class(rty) && self.cx.proxiable(rty) {
                self.proxy_recv = Some((def, targs.clone()));
            }
        }
        let mut argv = pre;
        let outer = std::mem::replace(&mut self.frozen_args, frozen != 0);
        let to_caller = std::mem::replace(&mut self.args_to_caller, plain);
        argv.extend(self.lower_args(args, &modes, true));
        self.args_to_caller = to_caller;
        self.frozen_args = outer;
        let ret = self.cx.subst(ret, &targs);
        let throws = throws.map(|e| self.cx.subst(e, &targs));
        let throws = self.cx.error_ty(throws);
        let r = self.finish_call(vir::Callee::Func(fid), argv, ret, throws);
        if plain && self.needs_drop(ret) {
            self.hand_to_reader(&r, ret);
        }
        r
    }

    /// Externs use the same mapping as Velt functions: aggregates by pointer (read-only borrow),
    /// aggregate results via a trailing out-pointer; a `never` result marks the extern noreturn.
    /// Types containing boxed values cross in their foreign layout (foreign.rs).
    fn extern_call(&mut self, def: DefId, args: &[hir::Expr]) -> Operand {
        let hir::Def::ExternFn(x) = self.cx.hir.def(def) else {
            ice("not an extern")
        };
        let foreign = x
            .params
            .iter()
            .chain([&x.ret])
            .any(|&t| self.cx.foreign_differs(t));
        if foreign && !x.is_async {
            return self.foreign_extern_call(x, args);
        }
        let mut params = vec![];
        let mut modes = vec![];
        for &p in &x.params {
            match self.cx.foreign_ty(p) {
                Ty::Unit => modes.push(PassMode::Copy),
                Ty::Agg(_) => {
                    params.push(Ty::Ptr);
                    modes.push(PassMode::Borrow);
                }
                s => {
                    params.push(s);
                    modes.push(PassMode::Copy);
                }
            }
        }
        let ret = match self.cx.ty(x.ret) {
            Ty::Agg(_) => {
                params.push(Ty::Ptr);
                Ty::Unit
            }
            t => t,
        };
        let never = self.cx.is_never(x.ret);
        let id = self.cx.extern_sym(&x.symbol, params, ret, never);
        let argv = match foreign {
            true => self.foreign_args(&x.params, args),
            false => self.lower_args(args, &modes, false),
        };
        self.finish_call(vir::Callee::Extern(id), argv, x.ret, None)
    }

    /// A synchronous extern whose signature mentions boxed values: arguments as foreign views,
    /// the result converted from the foreign layout.
    fn foreign_extern_call(&mut self, x: &hir::ExternFnDef, args: &[hir::Expr]) -> Operand {
        let mut params: Vec<Ty> = x
            .params
            .iter()
            .filter_map(|&p| match self.cx.foreign_ty(p) {
                Ty::Unit => None,
                Ty::Agg(_) => Some(Ty::Ptr),
                s => Some(s),
            })
            .collect();
        let mut argv = self.foreign_args(&x.params, args);
        let fret = self.cx.foreign_ty(x.ret);
        let ret = match fret {
            Ty::Agg(_) => Ty::Unit,
            t => t,
        };
        let out = (fret != Ty::Unit).then(|| self.temp(fret));
        if let (Some(o), Ty::Agg(_)) = (out, fret) {
            params.push(Ty::Ptr);
            argv.push(self.addr(Place::local(o)));
        }
        let never = self.cx.is_never(x.ret);
        let id = self.cx.extern_sym(&x.symbol, params, ret, never);
        let dest = out.filter(|_| ret != Ty::Unit).map(Place::local);
        self.call(vir::Callee::Extern(id), argv, dest, never);
        let Some(o) = out else { return unit() };
        let rt = self.sub(x.ret);
        let vt = self.cx.ty(rt);
        let native = self.temp(vt);
        self.adopt_foreign(&Place::local(o), rt, &Place::local(native));
        self.owned_result(Some(native), rt)
    }

    /// Arguments of an extern as foreign views (borrowed: the caller keeps ownership).
    fn foreign_args(&mut self, params: &[TyId], args: &[hir::Expr]) -> Vec<Operand> {
        let mut argv = vec![];
        for (&p, a) in params.iter().zip(args) {
            let v = self.expr(a);
            match self.cx.foreign_ty(p) {
                Ty::Unit => {}
                Ty::Agg(_) => {
                    let vt = self.cx.ty(p);
                    let place = self.operand_place(v, vt);
                    let view = self.foreign_view(&place, p);
                    argv.push(self.addr(view));
                }
                _ => argv.push(v),
            }
        }
        argv
    }

    /// Arguments of a direct call per the callee's modes. `user_code`: the callee may run user
    /// code (not a runtime extern), so borrows through counted objects are stabilized.
    pub(super) fn lower_args(
        &mut self,
        args: &[hir::Expr],
        modes: &[PassMode],
        user_code: bool,
    ) -> Vec<Operand> {
        if args.len() != modes.len() {
            ice("argument count does not match parameter count");
        }
        let mut argv: Vec<Operand> = vec![];
        let later = self.later_each(args);
        let outer = self.start_borrows();
        let mut recv = self.proxy_recv.take();
        for (i, ((a, mode), later)) in args.iter().zip(modes).zip(later).enumerate() {
            if i == 1 {
                if let (Some(r), Some(v)) = (recv.take(), argv.first()) {
                    self.proxy_recv_lowered(r, &v.clone());
                }
            }
            match (self.vty(a.ty), mode) {
                (Ty::Unit, _) => {
                    self.expr(a);
                }
                (Ty::Agg(_), PassMode::Borrow) if self.is_table(a) => {
                    // A table to a param that only reads it: its read-only data (#1334).
                    let place = self
                        .table_read(a)
                        .unwrap_or_else(|| ice("table without data"));
                    argv.push(self.addr(place));
                }
                (t @ Ty::Agg(_), PassMode::Borrow | PassMode::BorrowMut) => {
                    let v = match user_code {
                        true => self.stable_borrow(a, &args[i + 1..]),
                        false => self.borrowed_arg(a),
                    };
                    // A value the callee modifies stays where it is (it may only be shared). A
                    // frozen one changes for nobody, and is not counted (frozen.rs).
                    let v = match mode {
                        PassMode::Borrow if !self.frozen_root(a) => self.hold(v, a.ty, later),
                        _ => v,
                    };
                    argv.push(self.operand_addr(v, t));
                }
                (t @ Ty::Agg(_), PassMode::Owned | PassMode::Copy) => {
                    // A fresh copy the callee now owns (Owned) or only reads (Copy structs).
                    let v = match mode {
                        PassMode::Owned => self.owned_arg(a),
                        _ => self.consume(a),
                    };
                    let tmp = self.copy_to_temp(v, t);
                    argv.push(self.addr(Place::local(tmp)));
                }
                (_, PassMode::Owned) => {
                    let v = self.owned_arg(a);
                    argv.push(self.hold_owned(v, a.ty, later));
                }
                (_, m) => {
                    let v = match m {
                        PassMode::Borrow | PassMode::BorrowMut if user_code => {
                            let v = self.stable_borrow(a, &args[i + 1..]);
                            // A frozen one changes for nobody, and is not counted (frozen.rs);
                            // a number read out of one is still read now, before a later
                            // argument may free its box (a copy counts nothing).
                            let number = super::place::number(self.vty(a.ty));
                            match self.frozen_root(a) && !number {
                                true => v,
                                false => self.hold(v, a.ty, later),
                            }
                        }
                        _ => self.expr_held(a, later),
                    };
                    argv.push(v);
                }
            }
        }
        if let (Some(r), Some(v)) = (recv, argv.first()) {
            self.proxy_recv_lowered(r, &v.clone());
        }
        self.finish_borrows(outer);
        argv
    }

    /// Borrow-ABI arguments: every aggregate by pointer, the caller keeps ownership (a value
    /// moved into the call is dropped by the caller after it). `modes` (when the callee's param
    /// modes are known): Copy aggregates read out of a place are copied first when another
    /// argument (or the receiver: `receiver_mut`) is borrowed mutably, so no pointer argument
    /// aliases a `noalias` one. `transfer` (the call starts a spawned task): each argument is a
    /// copy for the task (transfer.rs).
    pub(super) fn borrow_args(
        &mut self,
        args: &[hir::Expr],
        modes: Option<&[PassMode]>,
        receiver_mut: bool,
        transfer: bool,
    ) -> (Vec<Operand>, Vec<Ty>) {
        let (mut argv, mut params) = (vec![], vec![]);
        let any_mut = receiver_mut || modes.is_some_and(|m| m.contains(&PassMode::BorrowMut));
        let later = self.later_each(args);
        let outer = self.start_borrows();
        for (i, (a, later)) in args.iter().zip(later).enumerate() {
            let t = self.vty(a.ty);
            let mode = modes.and_then(|m| m.get(i).copied());
            // A table (a module constant of number literals, #1334) to a param that only reads it:
            // its read-only data, not a new array (sema rejects every use that could write or
            // keep a table, and an array with `cap == 0` owns no buffer to free).
            let table = match (mode, transfer) {
                (Some(PassMode::Borrow), false) if self.is_table(a) => self.table_read(a),
                _ => None,
            };
            let v = if let Some(p) = table {
                Operand::Copy(p)
            } else if is_moved(a) {
                let v = self.consume(a);
                let ty = self.sub(a.ty);
                self.own_value(v, ty)
            } else {
                // A spawned call's borrows are settled right away, before the later arguments.
                let rest = if transfer { &[][..] } else { &args[i + 1..] };
                let v = self.stable_borrow(a, rest);
                if transfer {
                    // The copy for the task reads the argument now.
                    self.finish_borrows(Vec::new());
                }
                match mode {
                    // A value the callee modifies stays where it is; a frozen one changes for
                    // nobody, and is not counted (frozen.rs).
                    Some(PassMode::BorrowMut) if !t.is_scalar() => v,
                    _ if self.frozen_root(a) && !super::place::number(t) => v,
                    _ => self.hold(v, a.ty, later),
                }
            };
            let v = match transfer {
                true => self.transfer_copy(v, a.ty),
                false => v,
            };
            match t {
                Ty::Unit => {}
                Ty::Agg(_) => {
                    let v = if any_mut && mode != Some(PassMode::BorrowMut) && is_copy_read(a) {
                        let tmp = self.copy_to_temp(v, t);
                        Operand::Copy(Place::local(tmp))
                    } else {
                        v
                    };
                    argv.push(self.operand_addr(v, t));
                    params.push(Ty::Ptr);
                }
                s => {
                    argv.push(v);
                    params.push(s);
                }
            }
        }
        self.finish_borrows(outer);
        (argv, params)
    }

    /// Emit the call; aggregate/`Result` results go through an out-pointer temp. Owned results
    /// are registered as temporaries; throwing callees are checked (errors.rs).
    pub(super) fn finish_call(
        &mut self,
        callee: vir::Callee,
        mut argv: Vec<Operand>,
        ret: TyId,
        throws: Option<TyId>,
    ) -> Operand {
        let never = self.cx.is_never(ret) && throws.is_none();
        let abi = self.cx.ret_abi(ret, throws);
        if let Some(out) = abi.out {
            let tmp = self.temp(out);
            argv.push(self.addr(Place::local(tmp)));
            self.call(callee, argv, None, never);
            return match throws {
                Some(e) => self.check_result(tmp, ret, e),
                None => self.owned_result(Some(tmp), ret),
            };
        }
        match abi.ret {
            Ty::Unit => {
                self.call(callee, argv, None, never);
                unit()
            }
            s => {
                let d = self.temp(s);
                self.call(callee, argv, Some(Place::local(d)), never);
                self.owned_result(Some(d), ret)
            }
        }
    }

    /// After a call to a throwing function: route an error to the handler, else yield the Ok
    /// payload as an owned temporary.
    pub(super) fn check_result(&mut self, res: vir::Local, ret: TyId, err: TyId) -> Operand {
        let rty = self.cx.intern(TyKind::Result(ret, err));
        let rp = Place::local(res);
        let tag = self.rvalue_temp(
            Ty::U8,
            Rvalue::Use(Operand::Copy(proj(&rp, Proj::Field(0)))),
        );
        let is_err = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(vir::BinOp::Ne, tag, super::cint(0, Ty::U8)),
        );
        let err_bb = self.new_block();
        let ok_bb = self.new_block();
        self.branch(is_err, err_bb, ok_bb);
        self.switch_to(err_bb);
        let ev = self.cx.view(rty, 1);
        let payload = Operand::Copy(proj(&proj(&rp, Proj::Cast(ev)), Proj::Field(1)));
        self.fill_throw_loc();
        self.route_error(payload, err);
        self.switch_to(ok_bb);
        if self.cx.is_unit(ret) {
            return unit();
        }
        let ov = self.cx.view(rty, 0);
        let v = Operand::Copy(proj(&proj(&rp, Proj::Cast(ov)), Proj::Field(1)));
        self.own_value(v, ret)
    }
}

/// A place moved into the call (the caller drops it after the call).
pub(super) fn is_moved(a: &hir::Expr) -> bool {
    if let hir::ExprKind::Downcast(x) = &a.kind {
        return is_moved(x);
    }
    matches!(
        a.kind,
        hir::ExprKind::Local(_, UseMode::Move)
            | hir::ExprKind::Field {
                mode: UseMode::Move,
                ..
            }
            | hir::ExprKind::UnwrapSome(_, UseMode::Move)
            | hir::ExprKind::UnwrapVariant {
                mode: UseMode::Move,
                ..
            }
    )
}

/// A Copy value read out of a place (passed by pointer, it would still point into the place).
fn is_copy_read(a: &hir::Expr) -> bool {
    if let hir::ExprKind::Downcast(x) = &a.kind {
        return is_copy_read(x);
    }
    matches!(
        a.kind,
        hir::ExprKind::Local(_, UseMode::Copy)
            | hir::ExprKind::Field {
                mode: UseMode::Copy,
                ..
            }
            | hir::ExprKind::Index {
                mode: UseMode::Copy,
                ..
            }
            | hir::ExprKind::UnwrapSome(_, UseMode::Copy)
            | hir::ExprKind::UnwrapVariant {
                mode: UseMode::Copy,
                ..
            }
    )
}
