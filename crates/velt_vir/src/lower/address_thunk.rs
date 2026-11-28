//! The address of a std function that code calls through it on wasm32 (#3905): calls through an
//! address follow the C ABI the backends use, in which a pointer (and `size_t`) is a machine word,
//! 32 bits on wasm32. Velt's `usize` and `isize` are 64-bit on every target, so a function that
//! takes or returns one, such as the executor's poll and drop functions (`nodePoll(state: usize,
//! cx: usize)`) or the entries of its waker tables, would be an `(i64, i64)` function called as
//! `(i32, i32)`, which WebAssembly traps on. There, `fnAddress` gives the address of a thunk with
//! the machine-word signature, which widens each such argument, calls the function and narrows
//! the result. Memory needs nothing: a `Ptr` slot is 8 bytes on every target (vir.rs), and a
//! `usize` holding an address is the same word.
//!
//! Native targets, where the two widths are one, take the function's own address.

use velt_sema::hir::{self, DefId, ExprKind as E, IntTy, LocalId, TyId, TyKind, UseMode};

use super::c_call;
use super::{unit, Cx, FnLower, LInfo, LState, Work};
use crate::vir::{Function, Operand, Place, Rvalue, Terminator, Ty};

/// The machine word a `usize` or `isize` crosses as on wasm32, if `t` is one.
fn narrow_word(cx: &Cx<'_>, t: TyId) -> Option<(Ty, Ty)> {
    match cx.kind(t) {
        TyKind::Int(IntTy::USize) => Some((Ty::U32, Ty::U64)),
        TyKind::Int(IntTy::ISize) => Some((Ty::I32, Ty::I64)),
        _ => None,
    }
}

impl Cx<'_> {
    /// Does the executor in Velt run this target's programs? Natively and on WASI (its one
    /// thread); a JS host drives its turns itself and keeps the runtime's executor for now
    /// (#3905 step 2).
    pub(super) fn runs_std_executor(&self) -> bool {
        let t = self.target.trim();
        !t.starts_with("wasm") || c_call::wasi(t)
    }

    /// The function `fnAddress` names for `def` with `targs` (`fid`, its own) on this target: on
    /// wasm32, a thunk with machine words where it takes or returns `usize` or `isize`.
    pub(super) fn address_target(
        &mut self,
        def: DefId,
        targs: Vec<TyId>,
        fid: crate::vir::FuncId,
    ) -> crate::vir::FuncId {
        if !c_call::wasm32(&self.target) {
            return fid;
        }
        let f = self.fn_def(def);
        let (params, ret) = (f.params.iter().map(|p| p.ty).collect::<Vec<_>>(), f.ret);
        let subst: Vec<TyId> = params.iter().map(|&t| self.subst(t, &targs)).collect();
        let ret = self.subst(ret, &targs);
        let narrows = subst
            .iter()
            .chain([&ret])
            .any(|&t| narrow_word(self, t).is_some());
        if !narrows {
            return fid;
        }
        self.func(Work::AddressThunk(def, targs))
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    /// `Work::AddressThunk`: the machine-word form of `def` with `targs` (module docs).
    pub(super) fn build_address_thunk(cx: &'c mut Cx<'h>, def: DefId, targs: &[TyId]) -> Function {
        let f = cx.fn_def(def);
        let name = f.name.clone();
        let (span, ret) = (f.span, f.ret);
        let ptys: Vec<TyId> = f.params.iter().map(|p| p.ty).collect();
        let mut lw = FnLower::bare(cx, targs.to_vec());
        lw.loc = None;
        let ptys: Vec<TyId> = ptys.into_iter().map(|t| lw.sub(t)).collect();
        let ret = lw.sub(ret);
        // The thunk's parameters: machine words for `usize`/`isize`, the VIR type otherwise.
        let words: Vec<Option<(Ty, Ty)>> = ptys.iter().map(|&t| narrow_word(lw.cx, t)).collect();
        let params: Vec<Ty> = (ptys.iter().zip(&words))
            .map(|(&t, w)| w.map_or_else(|| lw.cx.ty(t), |(m, _)| m))
            .collect();
        let locals: Vec<_> = params.iter().map(|&t| lw.new_local(t, None)).collect();
        let mut info = vec![];
        let mut args = vec![];
        for (k, (&t, w)) in ptys.iter().zip(&words).enumerate() {
            let local = match w {
                // Widened as the integer it is: zero-extended for `usize`, sign-extended for
                // `isize` (VIR's casts extend by the source's signedness).
                Some((_, wide)) => {
                    let v = Operand::Copy(Place::local(locals[k]));
                    let w = lw.rvalue_temp(*wide, Rvalue::Cast(v, *wide));
                    let Operand::Copy(p) = w else {
                        super::ice("a widened thunk argument is not a temporary")
                    };
                    p.local
                }
                None => locals[k],
            };
            info.push(LInfo::new(Some(local), t, false, false, LState::Init));
            args.push(hir::Expr {
                kind: E::Local(LocalId(k as u32), UseMode::Copy),
                ty: t,
                span,
            });
        }
        lw.info = info;
        let call = hir::Expr {
            kind: E::Call {
                callee: hir::Callee::Def(def, targs.to_vec()),
                args,
            },
            ty: ret,
            span,
        };
        lw.push_scope(super::ScopeKind::Block);
        let v = lw.expr(&call);
        let (rty, v) = match narrow_word(lw.cx, ret) {
            Some((m, _)) => (m, lw.rvalue_temp(m, Rvalue::Cast(v, m))),
            None => match lw.cx.ty(ret) {
                Ty::Unit => (Ty::Unit, unit()),
                t => (t, lw.rvalue_temp(t, Rvalue::Use(v))),
            },
        };
        lw.terminate(Terminator::Return(v));
        lw.scopes.clear();
        let sym = format!("{}$addr", lw.cx.instance_symbol(&name, targs));
        lw.finish(sym, params, rty)
    }
}
