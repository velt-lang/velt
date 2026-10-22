//! What a `read` callback stores into the caller's containers and takes out of them
//! (rt_abi_async.md §9.1): places rooted in a variable the callback literal captured from code
//! outside read mode (frozen.rs `captured_root`, `Work::ReadOwn`). Those containers are counted
//! by the counts outside read mode, while the callback's own code counts by read mode's rules
//! (a lock-set object it did not make, the store's or the caller's, is borrowed uncounted).
//! Between the two:
//! - a value stored there that the caller holds already (a place of the caller's, or a
//!   reader's variable that borrows one: sema copies what a reader stores from the store,
//!   `Intrinsic::Transfer`) gets a reference by the counts outside read mode, so it is the
//!   same object, as in JavaScript; anything else becomes a copy of what the reader did not
//!   make (`reader_copy`), since it may be the store's;
//! - what such a container lets go (an element removed, a function's result) is counted out
//!   by those counts too, and freed only once the thread's reads end, since the reader may still
//!   borrow it uncounted (`hand_to_reader`, `LkMode::Deferred`).

use velt_sema::hir::{self, ExprKind as K, Intrinsic, TyId, TyKind};

use super::operand::proj;
use super::{FnLower, LkMode};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// The owned value of `value`, about to be stored into one of the caller's containers in a
    /// `read` callback (module docs).
    pub(super) fn into_caller(&mut self, value: &hir::Expr) -> Operand {
        if self.mode == LkMode::Plain {
            return self.consume(value);
        }
        let ty = self.sub(value.ty);
        let v = self.consume(value);
        if !held_by_caller(value) {
            return self.reader_copy(v, ty);
        }
        match self.lock_ref(ty) {
            Some(_) => self.from_reader(&v),
            None if self.cx.reaches_lock_set(ty) => return self.reader_copy(v, ty),
            None => {}
        }
        v
    }

    /// The owned value `v` (a lock-set object or none) the reader holds by read mode's rules,
    /// counted by the counts outside read mode: one the reader did not make, which it holds
    /// uncounted, gets a reference.
    fn from_reader(&mut self, v: &Operand) {
        if self.dead() {
            return;
        }
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Use(v.clone()));
        let nn = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, p.clone(), super::cint(0, Ty::Ptr)),
        );
        let (obj, done) = (self.new_block(), self.new_block());
        self.branch(nn, obj, done);
        self.switch_to(obj);
        let c = self.count_place(p.clone());
        let mine = self.is_local(&c);
        let count = self.new_block();
        self.branch(mine, done, count);
        self.switch_to(count);
        self.retain(p);
        self.goto(done);
        self.switch_to(done);
    }

    /// An owned argument of a direct call (`into_caller` when the callee keeps it in the
    /// caller's containers, `args_to_caller`).
    pub(super) fn owned_arg(&mut self, a: &hir::Expr) -> Operand {
        // Not for the arguments' own calls.
        match std::mem::take(&mut self.args_to_caller) {
            true => {
                let v = self.into_caller(a);
                self.args_to_caller = true;
                v
            }
            false => self.consume(a),
        }
    }

    /// Can a result of type `ty` of code changing the caller's containers be handed to the
    /// reader (`hand_to_reader`)? Not one holding a counted object outside the lock set that
    /// holds lock-set objects: read mode would leave those uncounted when it drops it.
    pub(super) fn hand_ok(&mut self, ty: TyId) -> bool {
        if !self.cx.reaches_lock_set(ty) || self.lock_ref(ty).is_some() {
            return true;
        }
        match self.cx.kind(ty) {
            TyKind::Array(e) | TyKind::Option(e, _) if self.inline(ty) => self.hand_ok(e),
            _ => false,
        }
    }

    /// Is a `ty` held inline (not behind a pointer)?
    fn inline(&mut self, ty: TyId) -> bool {
        matches!(self.cx.ty(ty), Ty::Agg(_))
    }

    /// The lock-set object type `ty` refers to, itself or as an option of one (a pointer).
    fn lock_ref(&mut self, ty: TyId) -> Option<TyId> {
        if self.cx.ty(ty) != Ty::Ptr {
            return None;
        }
        let t = match self.cx.kind(ty) {
            TyKind::Option(e, _) => e,
            _ => ty,
        };
        (self.cx.counted(t) && self.cx.lock_counted(t)).then_some(t)
    }

    /// The owned value `v` of type `ty`, which code changing the caller's containers returned
    /// (counted by the counts outside read mode; `hand_ok`), handed to the reader: each
    /// lock-set object it refers to that the reader did not make loses that reference by those
    /// counts, freed once the reads end when it was the last (`velt_rt_free_after_reads`), and
    /// the reader borrows it uncounted, as it borrows the store's. One the reader made
    /// (`LOCAL`) stays counted.
    pub(super) fn hand_to_reader(&mut self, v: &Operand, ty: TyId) {
        if self.dead() || self.mode == LkMode::Plain || !self.cx.reaches_lock_set(ty) {
            return;
        }
        let vt = self.cx.ty(ty);
        let p = match v {
            Operand::Copy(p) => p.clone(),
            v => Place::local(self.copy_to_temp(v.clone(), vt)),
        };
        self.hand_place(&p, ty);
    }

    fn hand_place(&mut self, p: &Place, ty: TyId) {
        if let Some(t) = self.lock_ref(ty) {
            return self.hand_ref(Operand::Copy(p.clone()), t);
        }
        match self.cx.kind(ty) {
            TyKind::Array(e) if self.inline(ty) => {
                let k = self.temp(Ty::U64);
                self.assign(Place::local(k), Rvalue::Use(super::cint(0, Ty::U64)));
                let len = Operand::Copy(proj(p, Proj::Field(1)));
                self.count_loop(k, len, |lw, k| {
                    let ep = lw.elem_place(p, k, e);
                    lw.hand_place(&ep, e);
                });
            }
            TyKind::Option(e, _) if self.inline(ty) => {
                let done = self.new_block();
                let some = self.option_is_some(p, ty);
                self.when(some, done);
                self.hand_place(&proj(p, Proj::Field(1)), e);
                self.goto(done);
                self.switch_to(done);
            }
            _ => {}
        }
    }

    /// `hand_to_reader` of a reference to a lock-set object of type `t` (or null).
    fn hand_ref(&mut self, v: Operand, t: TyId) {
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Use(v));
        let nn = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, p.clone(), super::cint(0, Ty::Ptr)),
        );
        let (obj, done) = (self.new_block(), self.new_block());
        self.branch(nn, obj, done);
        self.switch_to(obj);
        let c = self.count_place(p.clone());
        let mine = self.is_local(&c);
        let release = self.new_block();
        self.branch(mine, done, release);
        self.switch_to(release);
        let mode = std::mem::replace(&mut self.mode, LkMode::Deferred);
        self.release_of(p, t, false, |_| {});
        self.mode = mode;
        self.goto(done);
        self.switch_to(done);
    }
}

/// Is `e` a place the caller already holds, read without running code: a variable (the
/// caller's, or the reader's borrowing one; what a reader stores from the store sema copies,
/// `Intrinsic::Transfer`) and fields and elements at literal or variable indices?
fn held_by_caller(e: &hir::Expr) -> bool {
    match &e.kind {
        K::Local(..) => true,
        K::Field { base, .. } | K::UnwrapSome(base, _) | K::Upcast(base) | K::WrapSome(base) => {
            held_by_caller(base)
        }
        K::Index { base, index, .. } => {
            held_by_caller(base) && matches!(index.kind, K::Lit(_) | K::Local(..))
        }
        K::Call {
            callee: hir::Callee::Intrinsic(Intrinsic::Share),
            args,
        } if args.len() == 1 => held_by_caller(&args[0]),
        _ => false,
    }
}
