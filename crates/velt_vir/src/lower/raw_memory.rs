//! The intrinsics on raw memory and heap futures (docs/internals/design/intrinsics.md "Memory at
//! an address"): `addressStore`, `atomicAddress`, the address of a `Ptr<T>`'s integer for the
//! `ptrAtomic*` operations (atomic.rs emits them), and the calls through a heap future's header
//! (`futPoll`, `futDrop`, and every poll and drop of a heap future lowering emits).

use velt_sema::hir::{self, TyId, TyKind};

use super::operand::proj;
use super::{ice, unit, FnLower};
use crate::vir::{Callee, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// `addressStore<T>(address, value)`: `value` written at `address`, unchecked.
    pub(super) fn address_store(&mut self, a: &hir::Expr, v: &hir::Expr) -> Operand {
        let addr = self.expr(a);
        let t = self.vty(v.ty);
        if !t.is_scalar() {
            ice(format_args!("addressStore of VIR type {t:?}"));
        }
        let v = self.expr(v);
        let at = self.rvalue_temp(Ty::Ptr, Rvalue::Cast(addr, Ty::Ptr));
        let place = proj(&self.operand_place(at, Ty::Ptr), Proj::Deref(t));
        self.assign(place, Rvalue::Use(v));
        unit()
    }

    /// `atomicAddress(a)`: the address of the word of `a` (sema checked that it cannot move), as
    /// a value of type `ty` (`usize`).
    pub(super) fn atomic_address(&mut self, a: &hir::Expr, ty: TyId) -> Operand {
        let (p, _, _) = self.atomic_word(a);
        let t = self.vty(ty);
        self.rvalue_temp(t, Rvalue::Cast(p, t))
    }

    /// The address of the integer the `Ptr<T>` `p` points to, `T`'s VIR type and its width: the
    /// operands of a `ptrAtomic*` operation.
    pub(super) fn ptr_word(&mut self, p: &hir::Expr) -> (Operand, Ty, u32) {
        let pty = self.sub(p.ty);
        let vt = match self.cx.kind(pty) {
            TyKind::Adt(_, args) if args.len() == 1 => self.cx.ty(args[0]),
            k => ice(format_args!("ptrAtomic intrinsic on {k:?}")),
        };
        let bits = match vt {
            t if t.is_int() => t.scalar_size().unwrap_or(8) * 8,
            t => ice(format_args!(
                "ptrAtomic intrinsic on a Ptr to VIR type {t:?}"
            )),
        };
        let v = self.expr(p);
        let place = self.place_of(v, pty);
        // A `Ptr`'s one field is its address.
        let addr = self.field_place(&place, pty, 0);
        let at = self.rvalue_temp(Ty::Ptr, Rvalue::Cast(Operand::Copy(addr), Ty::Ptr));
        (at, vt, bits)
    }

    /// `futPoll(f, cx)` and `futDrop(f)`: the addresses as pointers, then the call through the
    /// header.
    pub(super) fn fut_intrinsic(&mut self, f: &hir::Expr, cx: Option<&hir::Expr>) -> Operand {
        let f = self.expr(f);
        let f = self.rvalue_temp(Ty::Ptr, Rvalue::Cast(f, Ty::Ptr));
        let Some(cx) = cx else {
            self.fut_drop_call(vec![f]);
            return unit();
        };
        let cx = self.expr(cx);
        let cx = self.rvalue_temp(Ty::Ptr, Rvalue::Cast(cx, Ty::Ptr));
        let r = self.temp(Ty::U32);
        self.fut_poll_call(vec![f, cx], Place::local(r));
        Operand::Copy(Place::local(r))
    }

    /// `statePoll(poll, state, cx)`: the call of the compiled poll function at address `poll`
    /// with `(state, cx)`, its `u32` result.
    pub(super) fn state_poll(
        &mut self,
        poll: &hir::Expr,
        state: &hir::Expr,
        cx: &hir::Expr,
    ) -> Operand {
        let [target, state, cx] = [poll, state, cx].map(|e| {
            let v = self.expr(e);
            self.rvalue_temp(Ty::Ptr, Rvalue::Cast(v, Ty::Ptr))
        });
        let callee = Callee::Ptr {
            target,
            params: vec![Ty::Ptr, Ty::Ptr],
            ret: Ty::U32,
        };
        let r = self.temp(Ty::U32);
        self.header_call(callee, vec![state, cx], Some(Place::local(r)));
        Operand::Copy(Place::local(r))
    }

    /// `stateDrop(drop, at)`: the call of the C function at address `drop` with `at`.
    pub(super) fn state_drop(&mut self, drop: &hir::Expr, at: &hir::Expr) -> Operand {
        let [target, at] = [drop, at].map(|e| {
            let v = self.expr(e);
            self.rvalue_temp(Ty::Ptr, Rvalue::Cast(v, Ty::Ptr))
        });
        let callee = Callee::Ptr {
            target,
            params: vec![Ty::Ptr],
            ret: Ty::Unit,
        };
        self.header_call(callee, vec![at], None);
        unit()
    }

    /// `addressCall(f, a)`: the call of the C function at address `f` with `a`, its
    /// pointer-sized result as a value of type `ty` (`usize`).
    pub(super) fn address_call(&mut self, f: &hir::Expr, a: &hir::Expr, ty: TyId) -> Operand {
        let [target, a] = [f, a].map(|e| {
            let v = self.expr(e);
            self.rvalue_temp(Ty::Ptr, Rvalue::Cast(v, Ty::Ptr))
        });
        let callee = Callee::Ptr {
            target,
            params: vec![Ty::Ptr],
            ret: Ty::Ptr,
        };
        let r = self.temp(Ty::Ptr);
        self.header_call(callee, vec![a], Some(Place::local(r)));
        let t = self.vty(ty);
        self.rvalue_temp(t, Rvalue::Cast(Operand::Copy(Place::local(r)), t))
    }

    /// `fnAddress(f)`: the address of the function `f` names (sema checked that C can call it),
    /// as a value of type `ty` (`usize`).
    pub(super) fn fn_address(&mut self, f: &hir::Expr, ty: TyId) -> Operand {
        let hir::ExprKind::FnRef(d, targs) = &f.kind else {
            ice("fnAddress of something other than a function's name")
        };
        let targs: Vec<TyId> = targs.iter().map(|&t| self.sub(t)).collect();
        let fid = self.cx.func_for_direct(*d, targs, super::LkMode::Plain);
        let t = self.vty(ty);
        let p = Operand::Const(crate::vir::Const::Func(fid), Ty::Ptr);
        self.rvalue_temp(t, Rvalue::Cast(p, t))
    }

    /// `f->poll(f, cx)` into `dest` (a `u32`, 1 when ready): the call of the function the heap
    /// future `f`'s header holds at offset 0 (rt_abi_async.md §1), with no runtime function in
    /// between.
    pub(in crate::lower) fn fut_poll_call(&mut self, args: Vec<Operand>, dest: Place) {
        let [f, cx] = <[Operand; 2]>::try_from(args)
            .unwrap_or_else(|_| ice("a heap future's poll takes the future and `cx`"));
        let f = self.fut_operand(f);
        let poll = self.fut_entry(&f, 0);
        let callee = Callee::Ptr {
            target: poll,
            params: vec![Ty::Ptr, Ty::Ptr],
            ret: Ty::U32,
        };
        self.header_call(callee, vec![f, cx], Some(dest));
    }

    /// `f->drop(f)`: the call of the function at offset 8 of the heap future `f`'s header, which
    /// cancels it if still running and frees it (never its result).
    pub(in crate::lower) fn fut_drop_call(&mut self, args: Vec<Operand>) {
        let [f] = <[Operand; 1]>::try_from(args)
            .unwrap_or_else(|_| ice("a heap future's drop takes the future"));
        let f = self.fut_operand(f);
        let drop = self.fut_entry(&f, 1);
        let callee = Callee::Ptr {
            target: drop,
            params: vec![Ty::Ptr],
            ret: Ty::Unit,
        };
        self.header_call(callee, vec![f], None);
    }

    /// `f` as an operand read twice (for the entry and as the argument): a constant or a
    /// local's value as it is, anything else copied first.
    fn fut_operand(&mut self, f: Operand) -> Operand {
        match &f {
            Operand::Copy(p) if p.proj.is_empty() => f,
            Operand::Const(..) => f,
            _ => self.rvalue_temp(Ty::Ptr, Rvalue::Use(f)),
        }
    }

    /// Entry `i` (0 poll, 1 drop) of the header of heap future `f`.
    fn fut_entry(&mut self, f: &Operand, i: u32) -> Operand {
        let header = self.cx.fut_header();
        let base = self.operand_place(f.clone(), Ty::Ptr);
        let entry = proj(&proj(&base, Proj::Deref(Ty::Agg(header))), Proj::Field(i));
        self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(entry)))
    }

    /// The call through a heap future's header. Unlike a call of a function value, it does not
    /// mark code in a `read` callback as calling an unknown function (`Boxing::read_indirect`):
    /// it calls the future's own poll or drop function, as the runtime function it replaces
    /// did.
    fn header_call(&mut self, callee: Callee, args: Vec<Operand>, dest: Option<Place>) {
        let before = self.cx.facts.read_indirect;
        self.call(callee, args, dest, false);
        self.cx.facts.read_indirect = before;
    }
}

impl super::Cx<'_> {
    /// The aggregate of a heap future's header, `{ poll, drop }` (16 bytes on every target: a
    /// pointer slot is 8 bytes on wasm32 too).
    pub(super) fn fut_header(&mut self) -> crate::vir::AggId {
        if let Some(a) = self.fut_header {
            return a;
        }
        let a = self.new_agg("VeltFut".into(), &[Ty::Ptr, Ty::Ptr]);
        self.fut_header = Some(a);
        a
    }
}
