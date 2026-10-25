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
use crate::lower::{cint, ice, FnLower};
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

/// The runtime's words at the start of an `RwLock<T>` (velt_rt `shared::RWLOCK_WORDS`).
const RWLOCK_WORDS: u32 = 19;

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
            (I::MutexNew | I::RwLockNew, [x]) => self.mutex_new(x, ty),
            (I::MutexWith, [m, f]) => self.mutex_with(m, f, ty, false),
            (I::LockRead, [m, f]) => self.mutex_with(m, f, ty, true),
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

    /// `new Mutex<T>(x)`: `{ lock: 0, value: x }` (an `RwLock`'s: its 19 runtime words, zero,
    /// consecutive, the value, then zero padding).
    fn mutex_new(&mut self, x: &hir::Expr, ty: TyId) -> Operand {
        let ty = self.sub(ty);
        let Ty::Agg(a) = self.cx.ty(ty) else {
            ice("Mutex<T> must be a struct `{ lock, value }`")
        };
        let v = self.consume(x);
        let at = self.lock_value_index(ty) as usize;
        let words = at;
        let n = self.cx.aggs[a.0 as usize].fields.len();
        let mut fields = vec![];
        let mut v = Some(v);
        for i in 0..n {
            match i == at {
                true => fields.push(v.take().unwrap_or_else(|| ice("a lock's value twice"))),
                false => {
                    let t = self.cx.aggs[a.0 as usize].fields[i].0;
                    fields.push(self.zero_value(t));
                }
            }
        }
        let m = self.temp(Ty::Agg(a));
        self.assign(Place::local(m), Rvalue::Aggregate(a, fields));
        let lock = self.addr(proj(&Place::local(m), Proj::Field(0)));
        let init = match self.cx.type_name(ty) == "RwLock" {
            true => {
                let offsets: Vec<u32> = self.cx.aggs[a.0 as usize].fields[..words]
                    .iter()
                    .map(|f| f.1)
                    .collect();
                if offsets != (0..RWLOCK_WORDS).map(|i| i * 8).collect::<Vec<u32>>() {
                    ice("RwLock<T> must start with its 19 consecutive lock words");
                }
                let vty = self.lock_value_ty(ty);
                self.cx.note_lock_value(vty);
                Rt::RwLockInit
            }
            false => Rt::MutexInit,
        };
        self.call_rt(init, vec![lock], None);
        self.owned_result(Some(m), ty)
    }

    /// `m.with(f)` / `rw.write(f)` (`read`: `rw.read(f)`): lock, `f(value)`, unlock; the result
    /// is `f`'s. `read` takes an `RwLock`'s lock shared, so readers run at once, unless its value
    /// can reach what read mode cannot protect (boxing/locks.rs): there the callback runs in
    /// velt_rt's read mode, which leaves the objects the readers share uncounted and stops a
    /// write to one.
    fn mutex_with(&mut self, m: &hir::Expr, f: &hir::Expr, ty: TyId, read: bool) -> Operand {
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
        let vty = self.lock_value_ty(mutex);
        let rw = self.cx.type_name(mutex) == "RwLock";
        if rw {
            self.cx.note_lock_value(vty);
        }
        let shared_read = read && rw && self.cx.shared_reads(vty);
        let (lock_fn, unlock_fn) = match (rw, shared_read) {
            // Code that may run in a `read` callback, where a `with` stops the program (velt_rt
            // `velt_rt_mutex_lock_checked`); code only run outside one locks as before.
            (false, _) if self.mode != crate::lower::LkMode::Plain => {
                (Rt::MutexLockChecked, Rt::MutexUnlock)
            }
            (false, _) => (Rt::MutexLock, Rt::MutexUnlock),
            (true, true) => (Rt::RwLockRead, Rt::RwLockReadUnlock),
            (true, false) => (Rt::RwLockWrite, Rt::RwLockWriteUnlock),
        };
        let lock = self.field_place(&mplace, mutex, 0);
        let lock = self.addr(lock);
        let at = self.lock_value_index(mutex);
        let value = self.field_place(&mplace, mutex, at);
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
        // A callback literal that took variables along into the lock (velt_sema
        // ownership/locked/moves.rs): its env lives in this frame (it is only called), what it
        // moved in is transferred before the lock is taken, and it is dropped before anything
        // leaves the lock, so every reference those variables held is released under it.
        // Only a `Mutex`'s: sema takes variables along for `with` alone, and an `RwLock`
        // callback's own captures keep their identity (velt_sema ownership/locked/moves.rs).
        let moved = match &f.kind {
            hir::ExprKind::Closure(d) if !rw => self.moved_captures(*d),
            _ => vec![],
        };
        if let (false, hir::ExprKind::Closure(d)) = (moved.is_empty(), &f.kind) {
            self.frame_closures.insert(*d);
        }
        if let hir::ExprKind::Closure(d) = &f.kind {
            let mode = match shared_read {
                true => crate::lower::LkMode::Read,
                false => crate::lower::LkMode::Plain,
            };
            self.lock_callback = Some((*d, mode));
        }
        let fv = self.borrowed_arg(f);
        self.lock_callback = None;
        let fp = self.place_of(fv.clone(), fty);
        if let hir::ExprKind::Closure(d) = &f.kind {
            if !moved.is_empty() {
                let ea = self.cx.env_agg(*d, &self.targs.clone());
                let env = proj(&proj(&fp, Proj::Field(1)), Proj::Deref(Ty::Agg(ea)));
                for &(field, ty) in &moved {
                    self.transfer_root_in_place(proj(&env, Proj::Field(field)), ty);
                }
            }
        }
        let code = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&fp, Proj::Field(0)))),
        );
        let env = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&fp, Proj::Field(1)))),
        );
        let ret = self.sub(ty);
        // An `RwLock`'s lock takes this thread's reader state (0: it looks it up) and says how
        // it went in, which its unlock takes back.
        let how = match rw {
            true => {
                let reader = self.rw_reader();
                let h = self.temp(Ty::U64);
                let args = vec![lock.clone(), Operand::Copy(Place::local(reader))];
                self.call_rt(lock_fn, args, Some(Place::local(h)));
                // The result without its tag (velt_rt `shared::HOW`).
                self.assign(
                    Place::local(reader),
                    Rvalue::Binary(
                        vir::BinOp::BitAnd,
                        Operand::Copy(Place::local(h)),
                        cint(-8, Ty::U64),
                    ),
                );
                Some(Operand::Copy(Place::local(h)))
            }
            false => {
                self.call_rt(lock_fn, vec![lock.clone()], None);
                None
            }
        };
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
        // The callback's environment, holding what it took along, is released under the lock;
        // released after it, it would race the next holder.
        if !moved.is_empty() {
            match &fv {
                Operand::Copy(p) if self.take_temp(p) => self.drop_glue(p.clone(), fty),
                _ => panic!("ICE: a `with` callback that takes variables along is not a temporary"),
            }
        }
        // A shared read's result leaves by read mode's rules whatever this code's mode: the
        // store's objects are uncounted there, so one the result shares is copied, never moved
        // out (it would look unique to the writers' counts).
        let mode = self.mode;
        if shared_read {
            self.mode = crate::lower::LkMode::Read;
        }
        let r = self.leave_lock(r, ret);
        self.mode = mode;
        // A function value stored into the value while the lock was held may be called by
        // another thread next (glue/many.rs).
        self.many_check(value, vty);
        let mut unlock_args = vec![lock];
        unlock_args.extend(how);
        self.call_rt(unlock_fn, unlock_args, None);
        r
    }

    /// This thread's `RwLock` reader state for `velt_rt_rwlock_read` (0: it looks it up), kept
    /// from the last read's result. Reset on every entry, a poll's too: a task may continue on
    /// another thread after an `await`.
    fn rw_reader(&mut self) -> crate::vir::Local {
        if let Some(r) = self.rw_reader {
            return r;
        }
        let r = self.temp(Ty::U64);
        let init = crate::vir::Stmt::Assign(Place::local(r), Rvalue::Use(cint(0, Ty::U64)));
        self.blocks[0].0.insert(0, init);
        self.block_locs[0].0.insert(0, None);
        self.rw_reader = Some(r);
        r
    }

    /// `T` of a `Mutex<T>` or `RwLock<T>`.
    fn lock_value_ty(&mut self, lock: TyId) -> TyId {
        let at = self.lock_value_index(lock) as usize;
        let tys = self.cx.adt_field_tys(lock);
        *tys.get(at).unwrap_or_else(|| ice("a lock without a value"))
    }

    /// The field of a `Mutex<T>` or `RwLock<T>` that holds the value: after a `Mutex`'s lock
    /// word, after an `RwLock`'s runtime words (padding follows it, the prelude).
    fn lock_value_index(&mut self, lock: TyId) -> u32 {
        match self.cx.type_name(lock) == "RwLock" {
            true => RWLOCK_WORDS,
            false => 1,
        }
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
