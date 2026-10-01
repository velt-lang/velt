//! Shared-state intrinsics (rt_abi_async.md §9):
//! - `shared<int>` `.add(n)` / `.get()` / `.set(v)`: atomics on the box's value cell
//!   (`{ count, value }`); the value must be a 64-bit integer (the rt atomics are `i64`).
//! - `Mutex<T>`: a struct whose field 0 is the 8-byte lock word and field 1 the value (the
//!   prelude declares it). `new Mutex(x)` zero-initializes the lock (`velt_rt_mutex_init`);
//!   `m.with(f)` on a `Mutex<T>` or `shared<Mutex<T>>` locks, calls `f(value)` through the
//!   closure borrow ABI, and unlocks. Closures cannot throw or await (POC), so the lock is
//!   released on every path that returns. The value is passed by pointer — also a scalar one
//!   when `f` is a closure literal (`Cx::by_ref_params`), so `(v) => { v += 1 }` updates it.

use velt_sema::hir::{self, Intrinsic, TyId, TyKind};

use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{ice, FnLower};
use crate::vir::{self, Operand, Place, Proj, Rvalue, Ty};

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
        self.call_rt(Rt::MutexLock, vec![lock.clone()], None);
        let (arg, pt) = match self.cx.ty(vty) {
            Ty::Agg(_) => (self.addr(value), Ty::Ptr),
            _ if by_ref => (self.addr(value), Ty::Ptr),
            s => (Operand::Copy(value), s),
        };
        let ret = self.sub(ty);
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
        self.call_rt(Rt::MutexUnlock, vec![lock], None);
        r
    }
}
