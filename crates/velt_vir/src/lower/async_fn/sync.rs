//! Shared-state intrinsics (rt_abi_async.md §9):
//! - `shared<int>` `.add(n)` / `.get()` / `.set(v)`: atomics on the box's value cell
//!   (`{ count, value }`); the value must be a 64-bit integer (the rt atomics are `i64`).
//! - A `shared<T>` read as its `T` (`Intrinsic::SharedRead`, #36): the box's value cell,
//!   borrowed, or a copy of it when the reader must not touch the counts in it.
//! - `Mutex<T>`: a struct whose field 0 is the 8-byte lock word and field 1 the value (the
//!   prelude declares it). `new Mutex(x)` zero-initializes the lock (`velt_rt_mutex_init`);
//!   `m.with(f)` on a `Mutex<T>` or `shared<Mutex<T>>` locks, calls `f(value)` through the
//!   closure borrow ABI, and unlocks. Closures cannot throw or await (POC), so the lock is
//!   released on every path that returns. The result leaves the lock transferred, and the
//!   value is checked for function values other threads could not call (`leave_lock`,
//!   glue/many.rs). The value is passed by pointer — also a scalar one
//!   when `f` is a closure literal (`Cx::by_ref_params`), so `(v) => { v += 1 }` updates it.
//!   A promise made under the lock would run after it is released: sema rejects those made
//!   from the value (velt_sema ownership/locked, which also transfers what the callback
//!   stores across the lock).

use velt_sema::hir::{self, Intrinsic, TyId, TyKind};

use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{ice, FnLower};
use crate::vir::{self, Operand, Place, Proj, Rvalue, Ty};

/// When a read of a `shared<T>` in its box retains the box for the statement. While the value or
/// a part of it is read there, code may run that gives the variable or object holding the handle
/// another one, which frees this box (a call the value or part is passed to, a loop over it, a
/// method it is the receiver of, an index or a later argument that calls something).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::lower) enum BoxHold {
    /// Nothing runs while it is read (a number read out of it, a length).
    No,
    /// Code may run: retained when something other than this statement can give the handle's
    /// place another handle (`handle_replaceable`).
    IfReplaceable,
    /// Code may run that this statement itself made able to (an index assigning the handle's
    /// variable): retained whatever the place.
    Yes,
}

impl FnLower<'_, '_> {
    pub(super) fn sync_intrinsic(&mut self, i: Intrinsic, args: &[hir::Expr], ty: TyId) -> Operand {
        use Intrinsic as I;
        match (i, args) {
            (I::SharedAdd, [s, n]) => {
                let (cell, vt) = self.shared_cell(s);
                let n = self.expr(n);
                let n = self.cast_to(n, vt, Ty::I64);
                let r = self.rt_scalar(Rt::AtomicAdd, vec![cell, n]);
                self.cast_to(r, Ty::I64, vt)
            }
            (I::SharedGet, [s]) => {
                let (cell, vt) = self.shared_cell(s);
                let r = self.rt_scalar(Rt::AtomicLoad, vec![cell]);
                self.cast_to(r, Ty::I64, vt)
            }
            (I::SharedSet, [s, v]) => {
                let (cell, vt) = self.shared_cell(s);
                let v = self.expr(v);
                let v = self.cast_to(v, vt, Ty::I64);
                self.call_rt(Rt::AtomicStore, vec![cell, v], None);
                crate::lower::unit()
            }
            (I::SharedRead, [s]) => self.shared_read(s),
            (I::MutexNew, [x]) => self.mutex_new(x, ty),
            (I::MutexWith, [m, f]) => self.mutex_with(m, f, ty),
            _ => ice(format_args!(
                "intrinsic {i:?} called with {} arguments",
                args.len()
            )),
        }
    }

    fn rt_scalar(&mut self, r: Rt, args: Vec<Operand>) -> Operand {
        let d = self.temp(r.sig().2);
        self.call_rt(r, args, Some(Place::local(d)));
        Operand::Copy(Place::local(d))
    }

    /// Address of the value cell of a `shared<int>` receiver, and the int's VIR type.
    fn shared_cell(&mut self, s: &hir::Expr) -> (Operand, Ty) {
        let sty = self.sub(s.ty);
        let TyKind::Shared(inner) = self.cx.kind(sty) else {
            ice("atomic operation on a non-shared value")
        };
        let vt = self.cx.ty(inner);
        if vt.scalar_size() != Some(8) || !vt.is_int() {
            ice("atomic operations need a 64-bit integer `shared` value");
        }
        let v = self.expr(s);
        let p = self.operand_place(v, Ty::Ptr);
        let bx = self.cx.shared_box(inner);
        let cell = proj(&proj(&p, Proj::Deref(Ty::Agg(bx))), Proj::Field(1));
        (self.addr(cell), vt)
    }

    /// The value inside the `shared<T>` that `s` evaluates to, in its box, retained for the
    /// statement as `hold` says ([`BoxHold`]; one atomic count change).
    fn shared_value(&mut self, s: &hir::Expr, hold: BoxHold) -> (Place, TyId) {
        let sty = self.sub(s.ty);
        let TyKind::Shared(inner) = self.cx.kind(sty) else {
            ice("SharedRead of a non-shared value")
        };
        let held = match hold {
            BoxHold::No => false,
            BoxHold::IfReplaceable => self.handle_replaceable(s),
            BoxHold::Yes => true,
        };
        let v = match held {
            true => {
                let v = self.expr(s);
                let held = self.share_value(v, sty);
                self.own_value(held, sty)
            }
            false => self.expr(s),
        };
        let p = self.operand_place(v, Ty::Ptr);
        let bx = self.cx.shared_box(inner);
        (
            proj(&proj(&p, Proj::Deref(Ty::Agg(bx))), Proj::Field(1)),
            inner,
        )
    }

    /// Can code other than the reader's own statement give the place `s` reads another
    /// `shared` handle? A variable a closure assigns, a module `let` some function reassigns,
    /// and anything reached through an object or array (whose other owners may replace it).
    fn handle_replaceable(&mut self, s: &hir::Expr) -> bool {
        use hir::ExprKind as K;
        match &s.kind {
            // A variable the body may give another handle (an assignment anywhere, a closure
            // assigning it by reference) is no cell, but the read keeps the box all the same.
            K::Local(l, _) => self.in_shared_cell(s) || self.body_changes(*l),
            K::Global(_) => self.in_shared_cell(s),
            K::Field { .. }
            | K::Index { .. }
            | K::UnwrapSome(..)
            | K::UnwrapVariant { .. }
            | K::Downcast(_) => true,
            _ => false,
        }
    }

    /// `Intrinsic::SharedRead`: the `T` of a `shared<T>`, borrowed in its box (#36). Other
    /// threads read it at the same time, so nothing here may change it (sema) or a reference
    /// count in it, which are not atomic: every path that would count it takes a copy instead
    /// (frozen.rs). When nothing in `T` is counted, the inline part (an array's or string's
    /// header) is read through a bitwise copy in a temporary that only borrows it (never
    /// dropped): in a local, the header stays in registers across the calls of a loop, where
    /// LLVM would reload it from the box after each one. Here (an argument, a receiver, a
    /// loop's source) code may run while it is read, see [`shared_value`](Self::shared_value);
    /// as the base of a projection, [`projection_base`](Self::projection_base).
    fn shared_read(&mut self, s: &hir::Expr) -> Operand {
        self.shared_read_in(s, BoxHold::IfReplaceable)
    }

    /// [`shared_read`](Self::shared_read), `runs_code` as for [`shared_value`](Self::shared_value).
    fn shared_read_in(&mut self, s: &hir::Expr, hold: BoxHold) -> Operand {
        let (place, inner) = self.shared_value(s, hold);
        let vt = self.cx.ty(inner);
        if vt == Ty::Unit || self.cx.holds_counted(inner) {
            return Operand::Copy(place);
        }
        let tmp = self.copy_to_temp(Operand::Copy(place), vt);
        Operand::Copy(Place::local(tmp))
    }

    /// `e` evaluated as the base of a projection (an element, a field, a length); `hold`:
    /// whether a `shared` box it reads in place is retained for the statement ([`BoxHold`]).
    /// For a base that is itself a projection, `proj_hold` hands `hold` to that projection's
    /// site, which takes it (place.rs).
    pub(in crate::lower) fn projection_base(&mut self, e: &hir::Expr, hold: BoxHold) -> Operand {
        use hir::ExprKind as K;
        match &e.kind {
            K::Call {
                callee: hir::Callee::Intrinsic(Intrinsic::SharedRead),
                args,
            } => self.shared_read_in(&args[0], hold),
            K::Field { .. } | K::Index { .. } => {
                self.proj_hold = Some(hold);
                let v = self.expr(e);
                // Not taken (a path that reads no `shared` box): never another site's.
                self.proj_hold = None;
                v
            }
            _ => self.expr(e),
        }
    }

    /// `Intrinsic::SharedRead` consumed (moved into a variable, an owned parameter, a field):
    /// a copy of the value inside, which the consumer owns.
    pub(in crate::lower) fn shared_read_owned(&mut self, s: &hir::Expr) -> Operand {
        let (place, inner) = self.shared_value(s, BoxHold::No);
        self.clone_value(Operand::Copy(place), inner)
    }

    /// `new Mutex<T>(x)`: `{ lock: 0, value: x }`.
    fn mutex_new(&mut self, x: &hir::Expr, ty: TyId) -> Operand {
        let ty = self.sub(ty);
        let Ty::Agg(a) = self.cx.ty(ty) else {
            ice("Mutex<T> must be a struct `{ lock, value }`")
        };
        let v = self.consume(x);
        let lock_ty = self.cx.aggs[a.0 as usize].fields[0].0;
        let zero = self.zero_value(lock_ty);
        let m = self.temp(Ty::Agg(a));
        self.assign(Place::local(m), Rvalue::Aggregate(a, vec![zero, v]));
        let lock = self.addr(proj(&Place::local(m), Proj::Field(0)));
        self.call_rt(Rt::MutexInit, vec![lock], None);
        self.owned_result(Some(m), ty)
    }

    /// `m.with(f)`: lock, `f(value)`, unlock; the result is `f`'s.
    fn mutex_with(&mut self, m: &hir::Expr, f: &hir::Expr, ty: TyId) -> Operand {
        let mty = self.sub(m.ty);
        let mv = self.expr(m);
        let mp = self.place_of(mv, mty);
        let (mplace, mutex) = match self.cx.kind(mty) {
            TyKind::Shared(inner) => {
                let bx = self.cx.shared_box(inner);
                (
                    proj(&proj(&mp, Proj::Deref(Ty::Agg(bx))), Proj::Field(1)),
                    inner,
                )
            }
            _ => (mp, mty),
        };
        let vty = self.cx.adt_field_tys(mutex)[1];
        let lock = self.field_place(&mplace, mutex, 0);
        let lock = self.addr(lock);
        let value = self.field_place(&mplace, mutex, 1);
        let fty = self.sub(f.ty);
        // A callback literal gets the value by pointer even when it is a scalar, so its updates
        // (`v += 1`) land in the mutex; other function values get scalars by value.
        let by_ref = match &f.kind {
            hir::ExprKind::Closure(d) => {
                let key = (*d, self.targs.clone());
                self.cx.by_ref_params.insert(key);
                true
            }
            _ => false,
        };
        let fv = self.borrowed_arg(f);
        let fp = self.place_of(fv, fty);
        let code = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&fp, Proj::Field(0)))),
        );
        let env = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&fp, Proj::Field(1)))),
        );
        let ret = self.sub(ty);
        self.call_rt(Rt::MutexLock, vec![lock.clone()], None);
        let (arg, pt) = match self.cx.ty(vty) {
            Ty::Agg(_) => (self.addr(value.clone()), Ty::Ptr),
            _ if by_ref => (self.addr(value.clone()), Ty::Ptr),
            s => (Operand::Copy(value.clone()), s),
        };
        let abi = self.cx.ret_abi(ret, None);
        let mut params = vec![Ty::Ptr, pt];
        if abi.out.is_some() {
            params.push(Ty::Ptr);
        }
        let callee = vir::Callee::Ptr {
            target: code,
            params,
            ret: abi.ret,
        };
        let r = self.finish_call(callee, vec![env, arg], ret, None);
        let r = self.leave_lock(r, ret);
        // A function value stored into the value while the lock was held may be called by
        // another thread next (glue/many.rs).
        self.many_check(value, vty);
        self.call_rt(Rt::MutexUnlock, vec![lock], None);
        r
    }

    /// The callback's result `r` (of type `ret`), about to leave the lock: transferred like a
    /// `spawn` argument while the lock is still held (#373), so nothing the mutex protects is
    /// referenced outside it — moved when the callback made it, deep-copied when it is part of
    /// the value. A promise is left alone: it runs on this task, and transferring it would
    /// start it under the lock.
    fn leave_lock(&mut self, r: Operand, ret: TyId) -> Operand {
        if self.dead()
            || matches!(self.cx.kind(ret), TyKind::Promise(..))
            || !self.cx.holds_counted(ret)
        {
            return r;
        }
        let owned = matches!(&r, Operand::Copy(p) if self.take_temp(&p.clone()));
        if !owned {
            return r;
        }
        let v = self.transfer_value(r, ret);
        self.own_value(v, ret)
    }
}
