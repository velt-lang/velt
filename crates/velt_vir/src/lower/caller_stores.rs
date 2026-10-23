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

use rustc_hash::FxHashSet;
use velt_sema::hir::{self, ExprKind as K, Intrinsic, LocalId, PassMode, TyId, TyKind};

use super::frozen::place_root;
use super::operand::proj;
use super::rt::Rt;
use super::{FnLower, Glue, LkMode, Work};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// The owned value of `value`, about to be stored into one of the caller's containers in a
    /// `read` callback (module docs).
    pub(super) fn keep_for_caller(&mut self, value: &hir::Expr) -> Operand {
        if self.mode == LkMode::Plain {
            return self.consume(value);
        }
        let ty = self.sub(value.ty);
        let held = self.held_by_caller(value);
        let v = self.consume(value);
        self.keep_value(v, held, ty)
    }

    /// `keep_for_caller` of the owned value `v` of type `ty`, already evaluated (`held`: its
    /// expression was `held_by_caller`).
    pub(super) fn keep_value(&mut self, v: Operand, held: bool, ty: TyId) -> Operand {
        if !held {
            return self.reader_copy(v, ty);
        }
        if !self.cx.reaches_lock_set(ty) {
            return v;
        }
        match self.lock_ref(ty) {
            Some(t) => self.count_for_caller(v, t, ty),
            None => self.reader_keep(v, ty),
        }
    }

    /// The owned value `v` (a lock-set object of type `t`, or none; `ty` is `v`'s type) the
    /// reader holds by read mode's rules, counted by the counts outside read mode: one the
    /// reader did not make, which it holds uncounted, gets a reference; one it made that may
    /// hold another's objects (uncounted) has them counted (`reader_keep`).
    fn count_for_caller(&mut self, v: Operand, t: TyId, ty: TyId) -> Operand {
        if self.dead() {
            return v;
        }
        let p = Place::local(self.copy_to_temp(v, Ty::Ptr));
        let pv = Operand::Copy(p.clone());
        let nn = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, pv.clone(), super::cint(0, Ty::Ptr)),
        );
        let (obj, done) = (self.new_block(), self.new_block());
        self.branch(nn, obj, done);
        self.switch_to(obj);
        let c = self.count_place(pv.clone());
        let mine = self.is_local(&c);
        let (made, count) = (self.new_block(), self.new_block());
        self.branch(mine, made, count);
        self.switch_to(count);
        self.retain(pv.clone());
        self.goto(done);
        self.switch_to(made);
        if self.holds_lock_set(t) {
            let kept = self.reader_keep(pv, ty);
            self.assign(p.clone(), Rvalue::Use(kept));
        }
        self.goto(done);
        self.switch_to(done);
        Operand::Copy(p)
    }

    /// The value `v` of type `ty` a reader stores into the caller's container, made of what the
    /// caller holds and what the reader made (`held_by_caller`): in read mode, the transfer glue
    /// moves what the reader made and alone holds, counts the objects it did not make (the
    /// caller's, which it held uncounted) once more and keeps them, and copies only what the
    /// reader made and still shares (velt_rt `velt_rt_xfer_keep_begin`).
    fn reader_keep(&mut self, v: Operand, ty: TyId) -> Operand {
        if self.dead() || !self.cx.transfers(ty) || !self.cx.holds_counted(ty) {
            return v;
        }
        let vt = self.cx.ty(ty);
        let t = Place::local(self.copy_to_temp(v, vt));
        let a = self.addr(t.clone());
        let f = self
            .cx
            .func_direct(Work::Glue(Glue::TransferRoot, ty), LkMode::Read);
        self.lk_split(
            |_| {},
            |lw| {
                lw.call_rt(Rt::XferKeepBegin, vec![], None);
                lw.call(vir::Callee::Func(f), vec![a], None, false);
                lw.call_rt(Rt::XferEnd, vec![], None);
            },
        );
        Operand::Copy(t)
    }

    /// Is `e` made of what the caller holds already and what the reader makes, read without
    /// running the reader's own code: a variable (the caller's, or the reader's holding the
    /// caller's objects or its own; what a reader stores from the store sema copies,
    /// `Intrinsic::Transfer`), fields and elements of one at literal or variable indices, what a
    /// method or function takes out of or reads from the caller's container (`out.pop()`,
    /// `m.get(k)`), literals, objects and closures made of these.
    pub(super) fn held_by_caller(&mut self, e: &hir::Expr) -> bool {
        match &e.kind {
            K::Local(..) | K::Lit(_) | K::Closure(_) => true,
            K::Field { base, .. }
            | K::UnwrapSome(base, _)
            | K::Upcast(base)
            | K::WrapSome(base) => self.held_by_caller(base),
            K::Index { base, index, .. } => {
                self.held_by_caller(base) && matches!(index.kind, K::Lit(_) | K::Local(..))
            }
            K::Call {
                callee: hir::Callee::Intrinsic(Intrinsic::Share),
                args,
            } if args.len() == 1 => self.held_by_caller(&args[0]),
            K::Call {
                callee: hir::Callee::Intrinsic(Intrinsic::Transfer),
                ..
            } => false,
            // From the caller's container, given nothing of the reader's that may be the
            // store's.
            K::Call {
                callee: hir::Callee::Def(..) | hir::Callee::Intrinsic(_),
                args,
            } => match args.split_first() {
                Some((c, rest)) => self.captured_root(c) && self.made_of_held(rest),
                None => false,
            },
            // `out.pop()!`: the payload, or a panic.
            K::Match { scrutinee, arms } => {
                self.held_by_caller(scrutinee)
                    && arms.iter().all(|a| {
                        let t = self.sub(a.body.ty);
                        self.cx.is_never(t) || self.held_by_caller(&a.body)
                    })
            }
            K::If { then, els, .. } => self.held_by_caller(then) && self.held_by_caller(els),
            K::ArrayLit(xs) | K::Tuple(xs) => self.made_of_held(xs),
            K::AdtLit { fields: xs, .. }
            | K::New { args: xs, .. }
            | K::Variant { args: xs, .. } => self.made_of_held(xs),
            _ => false,
        }
    }

    /// Is each of `xs` `held_by_caller`, or unable to hold a lock-set object?
    fn made_of_held(&mut self, xs: &[hir::Expr]) -> bool {
        xs.iter().all(|x| {
            let t = self.sub(x.ty);
            !self.cx.reaches_lock_set(t) || self.held_by_caller(x)
        })
    }

    /// An owned argument of a direct call (`keep_for_caller` when the callee keeps it in the
    /// caller's containers, `args_to_caller`).
    pub(super) fn owned_arg(&mut self, a: &hir::Expr) -> Operand {
        // Not for the arguments' own calls.
        match std::mem::take(&mut self.args_to_caller) {
            true => {
                let v = self.keep_for_caller(a);
                self.args_to_caller = true;
                v
            }
            false => self.consume(a),
        }
    }

    /// The arguments of a call changing the caller's containers (bit `i`: `args[i]`, given
    /// `modes`) that are the reader's containers it may change, which may hold the caller's
    /// objects (counted by read mode's rules: not at all) it could move into the caller's
    /// containers: `keep_mut_arg`.
    pub(super) fn kept_args_of(&mut self, args: &[hir::Expr], modes: &[PassMode]) -> u64 {
        let mut mask = 0;
        for (i, (a, m)) in args.iter().zip(modes).enumerate().take(64) {
            if !matches!(m, PassMode::Borrow | PassMode::BorrowMut) || self.captured_root(a) {
                continue;
            }
            let ty = self.sub(a.ty);
            // An object can be changed through a borrow; a value held inline only through a
            // mutable one (one only read is safe when it holds lock-set objects directly).
            let holds = match self.lock_ref(ty) {
                Some(t) => self.holds_lock_set(t),
                None => {
                    self.cx.reaches_lock_set(ty)
                        && !(*m == PassMode::Borrow && self.inline(ty) && self.hand_ok(ty))
                }
            };
            if holds {
                mask |= 1 << i;
            }
        }
        mask
    }

    /// The reader's container `a` (`kept_args_of`), passed to code counting by the counts
    /// outside read mode: first counted by those counts (`reader_keep`; the variable or part
    /// holding it holds that), and after the call handed back to the reader (`kept_handed`,
    /// `hand_to_reader`), which borrows what is in it uncounted from then on.
    pub(super) fn keep_mut_arg(&mut self, a: &hir::Expr) -> Operand {
        let ty = self.sub(a.ty);
        let vt = self.cx.ty(ty);
        if self.lock_ref(ty).is_none() && !self.hand_ok(ty) {
            return self.copy_mut_arg(a, ty, vt);
        }
        let p = match a.kind {
            K::Local(..) | K::Field { .. } | K::Index { .. } => {
                let p = self.place_expr(a);
                let was = self.own_once(&p, ty);
                let v = self.reader_keep(Operand::Copy(p.clone()), ty);
                self.assign(p.clone(), Rvalue::Use(v));
                self.kept_handed.push((Operand::Copy(p.clone()), ty, was));
                return match vt {
                    Ty::Agg(_) => self.addr(p),
                    _ => Operand::Copy(p),
                };
            }
            _ => {
                let v = self.consume(a);
                let v = self.reader_keep(v, ty);
                let p = Place::local(self.copy_to_temp(v, vt));
                // The call's own value: the reader drops it after handing it back (`Agg`; a
                // counted object is the reader's to borrow from then on).
                if matches!(vt, Ty::Agg(_)) {
                    self.kept_drops.push((p.clone(), ty, self.mode));
                }
                p
            }
        };
        self.kept_handed.push((Operand::Copy(p.clone()), ty, None));
        match vt {
            Ty::Agg(_) => self.addr(p),
            _ => Operand::Copy(p),
        }
    }

    /// Is the array at `p` (of type `ty`, a lock-set object) the reader's own, referenced once
    /// (`hand_back`)? Not asked for other types.
    fn own_once(&mut self, p: &Place, ty: TyId) -> Option<vir::Local> {
        if self.dead() || !matches!(self.cx.kind(ty), TyKind::Array(_)) || !self.cx.boxed(ty) {
            return None;
        }
        self.lock_ref(ty)?;
        let was = self.temp(Ty::Bool);
        self.assign(Place::local(was), Rvalue::Use(super::cint(0, Ty::Bool)));
        let o = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(p.clone())));
        let nn = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, o.clone(), super::cint(0, Ty::Ptr)),
        );
        let done = self.new_block();
        self.when(nn, done);
        let c = self.count_place(o);
        let one = super::cint(super::rc::LOCAL | 1, Ty::U64);
        let b = Rvalue::Binary(BinOp::Eq, Operand::Copy(c), one);
        self.assign(Place::local(was), b);
        self.goto(done);
        self.switch_to(done);
        Some(was)
    }

    /// After a call changing the caller's containers, the reader's container `v` of type `ty`
    /// it was given (`keep_mut_arg`) goes back to the reader. One that was the reader's own and
    /// referenced once (`was`, an array) and that the call did not keep anywhere (referenced
    /// once still) is the reader's own again, as in JavaScript: its place gets a new array of the
    /// reader's holding the same elements by read mode's counts, and the one the call saw is
    /// handed to the reader (freed once the reads end, letting go of what the keep counted in
    /// it). Anything else: `hand_to_reader`, the reader borrowing it uncounted.
    pub(super) fn hand_back(&mut self, v: &Operand, ty: TyId, was: Option<vir::Local>) {
        let (Some(was), Operand::Copy(p)) = (was, v) else {
            return self.hand_to_reader(v, ty);
        };
        if self.dead() || self.mode == LkMode::Plain {
            return;
        }
        let o = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(p.clone())));
        let (check, back, other, done) = (
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
        );
        self.branch(Operand::Copy(Place::local(was)), check, other);
        self.switch_to(check);
        let c = self.count_place(o.clone());
        let one = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, Operand::Copy(c), super::cint(1, Ty::U64)),
        );
        self.branch(one, back, other);
        self.switch_to(back);
        let op = Place::local(self.copy_to_temp(o.clone(), Ty::Ptr));
        let src = self.content(&op, ty);
        let len = Operand::Copy(proj(&src, Proj::Field(1)));
        let elem = self.elem_ty(ty);
        // Not the statement's to drop: the reader's place holds it.
        let new = self.with_capacity(len.clone(), ty);
        if let Operand::Copy(q) = &new {
            self.take_temp(q);
        }
        let np = Place::local(self.copy_to_temp(new, Ty::Ptr));
        let dst = self.content(&np, ty);
        let k = self.temp(Ty::U64);
        self.assign(Place::local(k), Rvalue::Use(super::cint(0, Ty::U64)));
        self.count_loop(k, len, |lw, k| {
            let ep = lw.elem_place(&src, k, elem);
            let s = lw.share_value(Operand::Copy(ep), elem);
            lw.push_value(&dst, elem, s);
        });
        self.assign(p.clone(), Rvalue::Use(Operand::Copy(np)));
        if let Some(t) = self.lock_ref(ty) {
            self.hand_ref(o, t);
        }
        self.goto(done);
        self.switch_to(other);
        self.hand_to_reader(v, ty);
        self.goto(done);
        self.switch_to(done);
    }

    /// The reader's container `a` of type `ty` (`vt`) that a call changing the caller's
    /// containers may change, which `hand_to_reader` cannot walk after the call (`hand_ok`; an
    /// object outside the lock set holding lock-set objects, as a class's own `Item[]`, or a
    /// `Map`): the call gets a copy of what the reader did not make (`reader_copy`),
    /// counted by the counts outside read mode and dropped by them after the call, so what it
    /// moves from there into the caller's containers is counted there. The reader's own
    /// container does not see the call's changes.
    fn copy_mut_arg(&mut self, a: &hir::Expr, ty: TyId, vt: Ty) -> Operand {
        let v = self.consume(a);
        let v = self.reader_copy(v, ty);
        let p = Place::local(self.copy_to_temp(v, vt));
        self.kept_drops.push((p.clone(), ty, LkMode::Deferred));
        match vt {
            Ty::Agg(_) => self.addr(p),
            _ => Operand::Copy(p),
        }
    }

    /// About to change `e`, a value held inline in an object (`o.items` in `o.items.push(x)`,
    /// or passed to be changed): stops a reader of an `RwLock` changing it in a lock-set object
    /// it did not make (rc.rs `write_barrier` of the nearest object holding it). A place read
    /// without running code has its parts above read again here; one reached by running code
    /// (`m.get(k)!.items`, `f().items`, `(c ? a : b).items`) gets the barrier while it is
    /// evaluated next, on the object it reaches (`owner_write`; the caller clears it after).
    pub(super) fn owner_barrier(&mut self, e: &hir::Expr) {
        if self.mode == LkMode::Plain || self.dead() {
            return;
        }
        let mut at = e;
        while let K::Field { base, .. } = &at.kind {
            if self.captured_root(base) {
                return;
            }
            if !self.pure_place(base) {
                self.owner_write = true;
                return;
            }
            let bty = self.sub(base.ty);
            if self.cx.lock_counted(bty) {
                let bp = self.place_expr(base);
                return self.target_barrier(&bp, bty);
            }
            if self.lock_ref(bty).is_some() || self.cx.counted(bty) {
                return;
            }
            at = base;
        }
    }

    /// In a field read of `base` (of type `bty`) about to be changed in place (`write`,
    /// `owner_barrier`): does the barrier go on `base`, the nearest lock-set object holding the
    /// field? A value held inline passes the question to the field read it is part of.
    pub(super) fn owner_write_down(&mut self, write: bool, base: &hir::Expr, bty: TyId) -> bool {
        if !write || self.caller_object(base) {
            return false;
        }
        if self.cx.lock_counted(bty) {
            return true;
        }
        if self.lock_ref(bty).is_none() && !self.cx.counted(bty) {
            self.owner_write = matches!(base.kind, K::Field { .. });
        }
        false
    }

    /// Is `e` one of the caller's objects or a part of one (module docs), as a place
    /// (`captured_root`) or what a method or function takes out of or reads from the caller's
    /// container given nothing that may reach the lock set (`out.at(0)!`, `out.find(f)`), or a
    /// choice of two of them?
    pub(super) fn caller_object(&mut self, e: &hir::Expr) -> bool {
        if self.captured_root(e) {
            return true;
        }
        match &e.kind {
            K::Field { base, .. }
            | K::Index { base, .. }
            | K::UnwrapSome(base, _)
            | K::Upcast(base) => self.caller_object(base),
            K::Call {
                callee: hir::Callee::Def(..) | hir::Callee::Intrinsic(_),
                args,
            } => match args.split_first() {
                Some((c, rest)) => {
                    self.caller_object(c) && rest.iter().all(|x| self.gives_none_of_value(x))
                }
                None => false,
            },
            // `out.at(0)!`: the payload, or a panic.
            K::Match { scrutinee, arms } => {
                self.caller_object(scrutinee)
                    && arms.iter().all(|a| {
                        let t = self.sub(a.body.ty);
                        self.cx.is_never(t)
                            || matches!(a.body.kind, K::Local(l, _) if binds(&a.pat, l))
                    })
            }
            K::If { then, els, .. } => self.caller_object(then) && self.caller_object(els),
            _ => false,
        }
    }

    /// Can the argument `x` give a callee nothing of the value: a type that cannot hold its
    /// objects, or a closure literal capturing only such variables and the caller's
    /// (`out.find((x) => x.v == k)`)?
    fn gives_none_of_value(&mut self, x: &hir::Expr) -> bool {
        let t = self.sub(x.ty);
        if !self.cx.reaches_lock_set(t) {
            return true;
        }
        let K::Closure(def) = x.kind else {
            return false;
        };
        let outer: Vec<LocalId> = self
            .cx
            .fn_def(def)
            .captures
            .iter()
            .map(|c| c.outer)
            .collect();
        outer.into_iter().all(|l| {
            let t = self.sub(self.info[l.0 as usize].ty);
            let e = hir::Expr {
                kind: K::Local(l, hir::UseMode::Borrow),
                ty: t,
                span: x.span,
            };
            !self.cx.reaches_lock_set(t) || self.captured_root(&e)
        })
    }

    /// The barrier `owner_write_down` asked for (`at`) on the object `bp` holds.
    pub(super) fn owner_write_barrier(&mut self, at: bool, bp: &Place, bty: TyId) {
        if at {
            self.target_barrier(bp, bty);
        }
    }

    /// Is `e` a place read without running code (variables, fields, elements at literal or
    /// variable indices)?
    fn pure_place(&self, e: &hir::Expr) -> bool {
        match &e.kind {
            K::Local(..) => true,
            K::Field { base, .. } => self.pure_place(base),
            K::Index { base, index, .. } => {
                self.pure_place(base) && matches!(index.kind, K::Lit(_) | K::Local(..))
            }
            _ => false,
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

    /// Is `l` a variable of this `read` callback naming one of the caller's objects: bound to
    /// a part of what the callback captured (`captured_root`, or of another such variable),
    /// directly or as a `for…of` element, and never given another value? Code changing what it
    /// names changes the caller's containers.
    pub(super) fn caller_alias(&mut self, l: LocalId) -> bool {
        if self.caller_aliases.is_none() {
            let found = match self.cx.cur_def.map(|d| self.cx.hir.def(d)) {
                Some(hir::Def::Fn(f)) => {
                    let mut b = Bindings {
                        captures: f.captures.iter().map(|c| c.inner).collect(),
                        ..Bindings::default()
                    };
                    b.block(&f.body.block);
                    b.aliases()
                }
                _ => FxHashSet::default(),
            };
            self.caller_aliases = Some(found);
        }
        self.caller_aliases.as_ref().is_some_and(|a| a.contains(&l))
    }
}

/// What `caller_alias` looks at in a body.
#[derive(Default)]
struct Bindings {
    captures: FxHashSet<LocalId>,
    /// Each variable bound to a place, with the place's root.
    bound: Vec<(LocalId, LocalId)>,
    /// Variables given a value more than once (assigned, or bound twice).
    changed: FxHashSet<LocalId>,
}

impl Bindings {
    fn bind(&mut self, l: LocalId, init: &hir::Expr) {
        if self.bound.iter().any(|(b, _)| *b == l) {
            self.changed.insert(l);
        }
        match place_root(unshared(init)) {
            Some(root) => self.bound.push((l, root)),
            None => {
                self.changed.insert(l);
            }
        }
    }

    fn aliases(&self) -> FxHashSet<LocalId> {
        let mut out = FxHashSet::default();
        loop {
            let before = out.len();
            for &(l, root) in &self.bound {
                if !self.changed.contains(&l)
                    && (self.captures.contains(&root) || out.contains(&root))
                {
                    out.insert(l);
                }
            }
            if out.len() == before {
                return out;
            }
        }
    }
}

impl Bindings {
    fn block(&mut self, b: &hir::Block) {
        b.stmts.iter().for_each(|s| self.stmt(s));
        if let Some(e) = &b.value {
            self.expr(e);
        }
    }

    fn stmt(&mut self, s: &hir::Stmt) {
        use hir::StmtKind as S;
        match &s.kind {
            S::Let { local, init } => {
                if let Some(init) = init {
                    self.bind(*local, init);
                    self.expr(init);
                }
            }
            S::LetPat { pat, init } => {
                if let hir::PatKind::Binding(l, _) = pat.kind {
                    self.bind(l, init);
                }
                self.expr(init);
            }
            S::Expr(init) | S::Return(Some(init)) => self.expr(init),
            S::Return(None) | S::Break(_) | S::Continue(_) => {}
            S::If { cond, then, els } => {
                self.expr(cond);
                self.block(then);
                els.iter().for_each(|b| self.block(b));
            }
            S::While {
                cond, body, step, ..
            } => {
                self.expr(cond);
                self.block(body);
                step.iter().for_each(|e| self.expr(e));
            }
            S::ForOf {
                binding,
                iter,
                body,
                ..
            } => {
                if let hir::PatKind::Binding(l, _) = binding.kind {
                    self.bind(l, iter);
                }
                self.expr(iter);
                self.block(body);
            }
            S::Try {
                body,
                catch,
                finally,
            } => {
                self.block(body);
                catch.iter().for_each(|(_, b)| self.block(b));
                finally.iter().for_each(|b| self.block(b));
            }
            S::Block(b) => self.block(b),
        }
    }

    fn expr(&mut self, e: &hir::Expr) {
        if let K::Assign { place, .. } | K::CompoundAssign { place, .. } = &e.kind {
            if let K::Local(l, _) = place.kind {
                self.changed.insert(l);
            }
        }
        match &e.kind {
            K::Block(b) => self.block(b),
            _ => super::frame_envs::children(e, &mut |x| self.expr(x)),
        }
    }
}

/// Does `p` bind `l`?
fn binds(p: &hir::Pat, l: LocalId) -> bool {
    use hir::PatKind as P;
    match &p.kind {
        P::Binding(b, _) => *b == l,
        P::Some(p) => binds(p, l),
        P::Variant { args: ps, .. } | P::Tuple(ps) | P::Or(ps) => ps.iter().any(|p| binds(p, l)),
        P::Adt { fields } => fields.iter().any(|(_, p)| binds(p, l)),
        P::Array { elems, rest } => *rest == Some(l) || elems.iter().any(|p| binds(p, l)),
        _ => false,
    }
}

/// `e` without a `Share` around it.
fn unshared(e: &hir::Expr) -> &hir::Expr {
    match &e.kind {
        K::Call {
            callee: hir::Callee::Intrinsic(Intrinsic::Share),
            args,
        } if args.len() == 1 => &args[0],
        _ => e,
    }
}
