//! Where a value a `read` callback stores goes, for what it made (rt_abi_async.md §9.1). The
//! transfer glue moves an object the reader made out of the callback and clears its `LOCAL`
//! bit unless the frame it goes to still runs in read mode (velt_rt `velt_rt_lk_leave`). A
//! callback's result goes to the code that called `read`, one callback out: by default the bit
//! stays when the thread is still in read mode after this callback (a nested `read`'s result,
//! the outer reader's own). A value sema transferred into a container (`Intrinsic::Transfer`)
//! goes to the frame that holds the container, found from where the store's destination was
//! captured: a variable of the callback around this one stays in read mode (`levels` 1), one
//! captured through `n` nested callbacks from code around them all goes `n` callbacks out, and
//! one the walk cannot follow (through a closure that is no `read` callback, or a container
//! reached through a variable of the callback's own) goes to code outside every read, as the
//! counts outside read mode hold it.

use rustc_hash::FxHashMap;
use velt_sema::hir::{self, DefId, ExprKind as K, Intrinsic, LocalId, TyId};

use super::frozen::place_root;
use super::rt::Rt;
use super::{cfunc, cint, FnLower, LkMode, Work};
use crate::vir::{BinOp, Operand, Place, Rvalue, Ty};

/// A frame outside every read (velt_rt `velt_rt_lk_leave_to`).
const OUTSIDE: u64 = u64::MAX;

impl FnLower<'_, '_> {
    /// Sema's transfer of `v` (of type `ty`, the `Intrinsic::Transfer` whose arguments are
    /// `args`): in a `read` callback that code in read mode may call, it moves into the frame
    /// holding the store's destination (module docs).
    pub(super) fn transfer_stored(&mut self, v: Operand, ty: TyId, args: &[hir::Expr]) -> Operand {
        let outermost = self.own_captures && !self.own_nested() && !self.own_deep;
        let levels = match self.mode == LkMode::Read && !outermost {
            true if self.cx.reaches_lock_set(ty) && self.cx.transfers(ty) => {
                self.store_levels(args.as_ptr() as usize)
            }
            _ => 1,
        };
        if levels == 1 || self.dead() {
            return self.transfer_value(v, ty);
        }
        let was = self.temp(Ty::U64);
        let to = vec![cint(i128::from(levels), Ty::U64)];
        self.call_rt(Rt::LkLeaveTo, to, Some(Place::local(was)));
        let v = self.transfer_value(v, ty);
        let back = vec![Operand::Copy(Place::local(was))];
        let ignored = self.temp(Ty::U64);
        self.call_rt(Rt::LkLeaveTo, back, Some(Place::local(ignored)));
        v
    }

    /// Is this a `read` callback nested in another one that code outside read mode calls
    /// (`Work::ReadOwn` of a literal in a `Work::ReadOwn` callback's body, closure.rs)? The
    /// caller's containers are then the variables it captured through each callback around it
    /// from the code outside them all (`caller_capture`).
    pub(super) fn own_nested(&self) -> bool {
        let parent = |d: Option<DefId>| d.and_then(|d| self.cx.closure_parents.get(&d).copied());
        self.own_captures
            && matches!(
                parent(parent(self.cx.cur_def).and_then(|p| p.0)),
                Some((_, true))
            )
    }

    /// Is `root`, a variable this `Work::ReadOwn` callback captured, one of the code outside
    /// every read around it: captured through each `read` callback around this one from the
    /// code declaring it, which calls the outermost (`own_nested`)? A variable of a callback
    /// around this one is that reader's, counted by read mode's rules.
    pub(super) fn caller_capture(&mut self, root: LocalId) -> bool {
        if !self.own_nested() {
            return true;
        }
        let Some(mut def) = self.cx.cur_def else {
            return false;
        };
        let mut var = root;
        loop {
            // The code calling the outermost callback, outside read mode for counting: its
            // variables are the caller's, declared there or captured by it (a closure
            // such as `const run = () => a.read(…)`).
            if !matches!(self.cx.closure_parents.get(&def), Some((_, true))) {
                return true;
            }
            let f = self.cx.fn_def(def);
            let Some(c) = f.captures.iter().find(|c| c.inner == var) else {
                return false;
            };
            match self.cx.closure_parents.get(&def) {
                Some(&(Some(p), true)) => (var, def) = (c.outer, p),
                _ => return false,
            }
        }
    }

    /// `f`, a transfer moving what this callback made into one of the caller's containers
    /// (caller_stores.rs): in a nested `read` callback (`own_nested`) that frame is outside
    /// every read, not the callback around this one (`velt_rt_lk_leave_to`).
    pub(super) fn in_caller_frame(&mut self, f: impl FnOnce(&mut Self)) {
        if !self.own_nested() && !self.own_deep {
            return f(self);
        }
        self.leave_to_outside(f);
    }

    /// The code of the `read` callback literal `def` (with captures) in code whose read mode is
    /// asked (`LkMode::Checked`: a closure, a poll): outside read mode that code is the caller
    /// whose variables the callback captured (`Work::ReadOwn`), in it a reader (`Work::Read`).
    pub(super) fn read_callback_code(&mut self, def: DefId) -> Operand {
        let work = Work::Fn(def, self.targs.clone());
        let own = cfunc(self.cx.func_read_own(work.clone(), false));
        let read = cfunc(self.cx.func_direct(work, LkMode::Read));
        let m = self.lk_mode();
        let reading = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, m, cint(0, Ty::U8)));
        let code = self.temp(Ty::Ptr);
        let (r, o, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(reading, r, o);
        self.switch_to(r);
        self.assign(Place::local(code), Rvalue::Use(read));
        self.goto(join);
        self.switch_to(o);
        self.assign(Place::local(code), Rvalue::Use(own));
        self.goto(join);
        self.switch_to(join);
        Operand::Copy(Place::local(code))
    }

    /// `f`, transfers moving what a `read` callback made into code outside every read (velt_rt
    /// `velt_rt_lk_leave_to(u64::MAX)`): in a nested callback (`in_caller_frame`), or the result
    /// of a `read` that a `Work::Deferred` instance calls (async_fn/sync.rs).
    pub(super) fn leave_to_outside(&mut self, f: impl FnOnce(&mut Self)) {
        if self.dead() {
            return f(self);
        }
        let was = self.temp(Ty::U64);
        let to = vec![cint(i128::from(OUTSIDE), Ty::U64)];
        self.call_rt(Rt::LkLeaveTo, to, Some(Place::local(was)));
        f(self);
        let back = vec![Operand::Copy(Place::local(was))];
        let ignored = self.temp(Ty::U64);
        self.call_rt(Rt::LkLeaveTo, back, Some(Place::local(ignored)));
    }

    /// Are the arguments `xs` of a call whose result a `read` callback stores into the caller's
    /// container (caller_stores.rs `held_by_caller`) the caller's places, also given owned
    /// (`first(mine)`), or values that cannot reach the lock set? Its result is then made of
    /// what the caller holds and what the call made, never of the store's objects.
    pub(super) fn callers_only(&mut self, xs: &[hir::Expr]) -> bool {
        xs.iter().all(|x| {
            let t = self.sub(x.ty);
            let x = match &x.kind {
                K::Call {
                    callee: hir::Callee::Intrinsic(Intrinsic::Share),
                    args,
                } if args.len() == 1 => &args[0],
                _ => x,
            };
            !self.cx.reaches_lock_set(t) || self.captured_root(x)
        })
    }

    /// The frame the transfer whose arguments start at `key` stores into (module docs).
    fn store_levels(&mut self, key: usize) -> u64 {
        if self.leave_levels.is_none() {
            let mut stores = Stores::default();
            if let Some(hir::Def::Fn(f)) = self.cx.cur_def.map(|d| self.cx.hir.def(d)) {
                stores.block(&f.body.block);
            }
            let mut found = FxHashMap::default();
            for (keys, roots) in stores.found {
                // The places that could hold what the reader made.
                let roots: Vec<LocalId> = roots
                    .into_iter()
                    .filter(|&(_, t)| {
                        let t = self.sub(t);
                        self.cx.reaches_lock_set(t)
                    })
                    .map(|(r, _)| r)
                    .collect();
                let levels = roots.iter().map(|&r| self.root_levels(r)).max();
                let levels = levels.unwrap_or(OUTSIDE);
                for k in keys {
                    found.insert(k, levels);
                }
            }
            self.leave_levels = Some(found);
        }
        let levels = self
            .leave_levels
            .as_ref()
            .and_then(|m| m.get(&key).copied());
        levels.unwrap_or(OUTSIDE)
    }

    /// How many `read` callbacks out from this one the frame holding the variable `root` of
    /// this callback is: through the captures of each callback around it (`closure_parents`),
    /// up to the one declaring it.
    fn root_levels(&mut self, root: LocalId) -> u64 {
        let Some(mut def) = self.cx.cur_def else {
            return OUTSIDE;
        };
        let (mut var, mut levels) = (root, 0u64);
        loop {
            let callback = matches!(self.cx.closure_parents.get(&def), Some((_, true)));
            let f = self.cx.fn_def(def);
            let Some(c) = f.captures.iter().find(|c| c.inner == var) else {
                // Declared here: a callback's own variable (a reader's container; not this
                // callback's, whose own variables name what the walk cannot follow), or one of
                // the code around a chain of `Work::ReadOwn` callbacks, outside every read
                // however deep the chain starts (`own_deep`).
                return match levels {
                    0 => OUTSIDE,
                    _ if !callback && self.own_captures => OUTSIDE,
                    n => n,
                };
            };
            let parent = self.cx.closure_parents.get(&def).and_then(|p| p.0);
            let (true, Some(p)) = (callback, parent) else {
                return OUTSIDE;
            };
            (var, levels, def) = (c.outer, levels + 1, p);
        }
    }
}

/// Each `Intrinsic::Transfer` a store gives a value (the address of its arguments), with the
/// roots of the places the store may put it in: an assignment's target, the other arguments
/// of a call (`s.add(v)`, `stash(out, v)`; also through a rest parameter's array literal).
#[derive(Default)]
struct Stores {
    found: Vec<(Vec<usize>, Vec<Root>)>,
}

/// A place's root variable, with the type of what the place holds.
type Root = (LocalId, TyId);

impl Stores {
    fn block(&mut self, b: &hir::Block) {
        b.stmts.iter().for_each(|s| self.stmt(s));
        if let Some(e) = &b.value {
            self.expr(e);
        }
    }

    fn stmt(&mut self, s: &hir::Stmt) {
        use hir::StmtKind as S;
        match &s.kind {
            S::Let { init, .. } => init.iter().for_each(|e| self.expr(e)),
            S::LetPat { init, .. } | S::Expr(init) | S::Return(Some(init)) => self.expr(init),
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
            S::ForOf { iter, body, .. } => {
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
        match &e.kind {
            K::Assign { place, value } => {
                let keys = transfers_in(std::slice::from_ref(&**value));
                if !keys.is_empty() {
                    let root = place_root(place).map(|r| (r, place.ty));
                    self.found.push((keys, root.into_iter().collect()));
                }
            }
            // A transfer is no store: the store it is part of names its destination.
            K::Call {
                callee: hir::Callee::Intrinsic(Intrinsic::Transfer),
                ..
            } => {}
            K::Call { args, .. } => {
                let keys = transfers_in(args);
                if !keys.is_empty() {
                    let roots = args
                        .iter()
                        .filter_map(|a| Some((place_root(a)?, a.ty)))
                        .collect();
                    self.found.push((keys, roots));
                }
            }
            _ => {}
        }
        match &e.kind {
            K::Block(b) => self.block(b),
            _ => super::frame_envs::children(e, &mut |x| self.expr(x)),
        }
    }
}

/// The transfers among `xs` (the address of each one's arguments), also as the elements of an
/// array literal.
fn transfers_in(xs: &[hir::Expr]) -> Vec<usize> {
    let mut out = vec![];
    for x in xs {
        match &x.kind {
            K::Call {
                callee: hir::Callee::Intrinsic(Intrinsic::Transfer),
                args,
            } => {
                // Sema may transfer a transfer (a stored `slice()`): both go to the same frame.
                out.push(args.as_ptr() as usize);
                out.extend(transfers_in(args));
            }
            K::ArrayLit(es) => out.extend(transfers_in(es)),
            _ => {}
        }
    }
    out
}
