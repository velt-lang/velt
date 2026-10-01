//! Borrow-ABI entry points. Dynamic calls (function values, virtual and interface methods) pass
//! `(first: ptr, args…, [out])` with every aggregate argument borrowed by pointer. A thunk
//! adapts a function instance to that shape:
//! - `Env`: `first` is a closure env the target (a named function) does not take;
//! - `SelfData`: `first` is the receiver's data pointer; the target's `this` is that pointer
//!   (aggregates, class objects) or the value loaded from it (scalars).
//!
//! In both, a target param that takes ownership (`PassMode::Owned` of a droppable type) gets a
//! deep copy, since the caller keeps its argument. Methods need no thunk when neither applies.

use velt_sema::hir::{DefId, PassMode, TyId};

use crate::lower::{Cx, FnLower, ScopeKind, ThunkKind, Work};
use crate::vir::{self, FuncId, Function, Operand, Place, Proj, Terminator, Ty};

/// How a thunk forwards one incoming argument to its target.
enum Incoming {
    Pass,
    /// The target takes ownership: pass a deep copy.
    Owned,
}

impl Cx<'_> {
    /// Entry point for calling method `def<targs>` with the receiver's data pointer first.
    pub(in crate::lower) fn self_entry(&mut self, def: DefId, targs: Vec<TyId>) -> FuncId {
        let f = self.fn_def(def);
        let this = f.params.first().map(|p| p.ty);
        let owned: Vec<(PassMode, TyId)> =
            f.params.iter().skip(1).map(|p| (p.mode, p.ty)).collect();
        let this_scalar = match this {
            Some(t) => {
                let t = self.subst(t, &targs);
                !matches!(self.ty(t), Ty::Agg(_) | Ty::Ptr)
            }
            None => true,
        };
        let needs_copy = owned.into_iter().any(|(m, t)| {
            let t = self.subst(t, &targs);
            m == PassMode::Owned && self.needs_drop(t)
        });
        if this_scalar || needs_copy {
            self.func(Work::Thunk(ThunkKind::SelfData, def, targs))
        } else {
            self.func_for(def, targs)
        }
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    /// A deep copy of a borrowed incoming argument (param local `l`), passed as owned.
    fn owned_copy(&mut self, l: vir::Local, vt: Ty, pty: TyId) -> Operand {
        let src = match vt {
            Ty::Agg(_) => Operand::Copy(Place {
                local: l,
                proj: vec![Proj::Deref(vt)],
            }),
            _ => Operand::Copy(Place::local(l)),
        };
        let c = self.clone_value(src, pty);
        match vt {
            Ty::Agg(_) => self.operand_addr(c, vt),
            _ => c,
        }
    }

    pub(in crate::lower) fn build_thunk(
        cx: &'c mut Cx<'h>,
        kind: ThunkKind,
        def: DefId,
        targs: &[TyId],
    ) -> Function {
        let f = cx.fn_def(def);
        let mut lw = FnLower::bare(cx, targs.to_vec());
        let first = lw.new_local(Ty::Ptr, Some("first".into()));
        let mut params = vec![Ty::Ptr];
        let mut args = vec![];
        let skip = match kind {
            ThunkKind::Env(_) => 0,
            ThunkKind::SelfData => 1,
        };
        if kind == ThunkKind::SelfData {
            let this = f
                .params
                .first()
                .map(|p| p.ty)
                .unwrap_or_else(|| crate::lower::ice("method without this"));
            let tt = lw.sub(this);
            match lw.cx.ty(tt) {
                Ty::Agg(_) | Ty::Ptr => args.push(Operand::Copy(Place::local(first))),
                Ty::Unit => {}
                s => args.push(Operand::Copy(Place {
                    local: first,
                    proj: vec![Proj::Deref(s)],
                })),
            }
        }
        // Params (and the out-pointer) must be the first locals; adapt them afterwards.
        let mut incoming = vec![];
        let by_ref = lw.cx.by_ref_params.contains(&(def, targs.to_vec()));
        for p in f.params.iter().skip(skip) {
            let pty = lw.sub(p.ty);
            let vt = lw.cx.ty(pty);
            if vt == Ty::Unit {
                continue;
            }
            let by_ptr = matches!(vt, Ty::Agg(_)) || by_ref;
            let pt = if by_ptr { Ty::Ptr } else { vt };
            let l = lw.new_local(pt, None);
            params.push(pt);
            let adapt = if p.mode == PassMode::Owned && lw.cx.needs_drop(pty) {
                Incoming::Owned
            } else {
                Incoming::Pass
            };
            incoming.push((l, vt, pty, adapt));
        }
        let (ret, throws) = lw.cx.call_sig(f);
        let ret = lw.sub(ret);
        let throws = throws.map(|e| lw.sub(e));
        let throws = lw.cx.error_ty(throws);
        let own = match kind {
            ThunkKind::Env(Some(e)) => lw.cx.error_ty(Some(e)),
            _ => throws,
        };
        let abi = lw.cx.ret_abi(ret, own);
        let out = abi.out.map(|_| {
            params.push(Ty::Ptr);
            lw.new_local(Ty::Ptr, Some("ret.out".into()))
        });
        for (l, vt, pty, adapt) in incoming {
            args.push(match adapt {
                Incoming::Owned => lw.owned_copy(l, vt, pty),
                Incoming::Pass => Operand::Copy(Place::local(l)),
            });
        }
        let target = lw.cx.func_for(def, targs.to_vec());
        let tag = match kind {
            ThunkKind::Env(Some(e)) => format!("env_E{}", e.0),
            ThunkKind::Env(None) => "env".into(),
            ThunkKind::SelfData => "self".into(),
        };
        let sym = format!("_Gthunk_{tag}_{}", lw.cx.instance_symbol(&f.name, targs));
        if own != throws {
            lw.adapt_errors(target, args, (ret, throws, own), out);
            return lw.finish(sym, params, abi.ret);
        }
        if let Some(out) = out {
            args.push(Operand::Copy(Place::local(out)));
        }
        let r = match abi.ret {
            Ty::Unit => {
                lw.call(vir::Callee::Func(target), args, None, false);
                crate::lower::unit()
            }
            t => {
                let d = lw.temp(t);
                lw.call(
                    vir::Callee::Func(target),
                    args,
                    Some(Place::local(d)),
                    false,
                );
                Operand::Copy(Place::local(d))
            }
        };
        lw.terminate(Terminator::Return(r));
        lw.finish(sym, params, abi.ret)
    }

    /// Thunk body calling `target` (which throws `inner`) and returning its result as a
    /// function throwing `own` (a wider error type): errors are converted, values wrapped `Ok`.
    fn adapt_errors(
        &mut self,
        target: FuncId,
        args: Vec<Operand>,
        (ret, inner, own): (TyId, Option<TyId>, Option<TyId>),
        out: Option<vir::Local>,
    ) {
        self.ret_ty = Some(ret);
        self.throws = own;
        self.out_ptr = out;
        self.push_scope(ScopeKind::Block);
        let v = self.finish_call(vir::Callee::Func(target), args, ret, inner);
        let v = match self.cx.is_unit(ret) {
            true => None,
            false => {
                if let Operand::Copy(p) = &v {
                    self.take_temp(p);
                }
                Some(v)
            }
        };
        self.emit_return(v);
        self.scopes.clear();
    }
}
