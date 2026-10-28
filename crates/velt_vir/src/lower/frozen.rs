//! Frozen values: a value other threads read at the same time, which this code must neither
//! change nor count (#36, #708). It is the value inside a `shared<T>` (`Intrinsic::SharedRead`),
//! a param of a `Work::Frozen` instance in its mask, a binding by reference into either, or a
//! part of one of those (a *frozen root*). Sema rejects every write to one, also through the
//! functions it is passed to. Lowering must not change a reference count in one either, since
//! those are not atomic (semantics stage 2 §6):
//! - a stabilized borrow (`stable_borrow`) and a held operand (`hold`) of one take no share and
//!   retain nothing on the way: nothing changes a frozen value, and where code could give the
//!   variable or object holding its `shared` handle another one while it is read, the reader
//!   retains the box (async_fn/sync.rs `shared_value`), so it stays where it is;
//! - one consumed (moved into a variable, an owned param, a field, returned) is a deep copy
//!   (`FnLower::consume`);
//! - one passed to a borrowed param of a direct call goes to the callee's `Work::Frozen`
//!   instance with that param in its mask, if it holds counted parts or is an object or array
//!   code could change (`frozen_counted`); through a function value, whose code cannot be
//!   specialized, it is a copy (`borrow_args`), since the function may change what it is given;
//! - a binding by reference into one, a `for…of` element of one and a borrowed capture of one
//!   are frozen too (the capture through a `Work::Frozen` instance of the closure), and so are
//!   a map loop's hidden map and its entries when the body changes neither
//!   (`frozen_map_entry`);
//! - a closure literal passed to a frozen instance is called there directly, through its own
//!   frozen instance ([`KnownFn`]), so a callback reads what it is given in place.
//!
//! A number or string holds nothing a callee could count or change: it is borrowed as usual and
//! goes to ordinary instances.

use rustc_hash::FxHashSet;

use velt_sema::hir::{
    self, Callee, ExprKind as K, Intrinsic, LocalId, PassMode, Pat, PatKind, TyKind, UseMode,
};

use super::operand::proj;
use super::{cfunc, FnLower, Work};
use crate::vir::{Operand, Place, Proj, Rvalue, Ty};

/// A closure literal a `Work::Frozen` instance was given for function param `param`: closure
/// `def` at `targs`, whose code is the instance with frozen captures `captures` (closure.rs).
/// A call of the param there goes straight to the closure's code with the frozen arguments in
/// its own mask, so a callback (`xs.some((x) => …)`) reads the elements it is given without
/// copies, as a direct call would.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct KnownFn {
    pub param: u32,
    pub def: hir::DefId,
    pub targs: Vec<hir::TyId>,
    pub captures: u64,
}

impl FnLower<'_, '_> {
    /// Is `e` a frozen root (module docs), or a part of one?
    pub(super) fn frozen_root(&mut self, e: &hir::Expr) -> bool {
        // A copy of an object type's fields is a new object of this thread's.
        if self.part_copy_upcast(e) {
            return false;
        }
        match &e.kind {
            K::Call {
                callee: Callee::Intrinsic(Intrinsic::SharedRead),
                ..
            } => true,
            K::Local(l, _) => self.frozen.contains(l),
            K::Field { base, .. }
            | K::Index { base, .. }
            | K::UnwrapSome(base, _)
            | K::UnwrapVariant { expr: base, .. }
            | K::Downcast(base)
            | K::Upcast(base) => self.frozen_root(base),
            _ => false,
        }
    }

    /// Is `e` frozen, and is it a value code could count (counted parts) or change (an object
    /// or array, through a function value it reaches: `borrow_args` gives those a copy) rather
    /// than a number or string, which a callee only reads?
    pub(super) fn frozen_counted(&mut self, e: &hir::Expr) -> bool {
        if !self.frozen_root(e) {
            return false;
        }
        let ty = self.sub(e.ty);
        let vt = self.vty(e.ty);
        self.cx.holds_counted(ty)
            || !(super::place::number(vt) || matches!(self.cx.kind(ty), TyKind::Str))
    }

    /// The mask of a direct call's params that get a frozen argument it could count or change
    /// (`frozen_counted`; `args` follow
    /// `skip` already lowered leading params): such a call goes to a `Work::Frozen` instance.
    pub(super) fn frozen_mask(
        &mut self,
        modes: &[PassMode],
        args: &[hir::Expr],
        skip: usize,
    ) -> u64 {
        let mut mask = 0u64;
        for (i, (a, m)) in args.iter().zip(modes).enumerate() {
            let at = i + skip;
            if *m == PassMode::Borrow && at < 64 && self.frozen_counted(a) {
                mask |= 1 << at;
            }
        }
        mask
    }

    /// A stabilized borrow of the frozen argument `a` (`stable_borrow`): the value itself for a
    /// frozen param of the callee (`frozen_args`) or one that holds no counted part, else a copy
    /// the call borrows (dropped with the statement).
    pub(super) fn frozen_borrow(&mut self, a: &hir::Expr) -> Operand {
        let ty = self.sub(a.ty);
        let v = self.frozen_read(a);
        if !self.frozen_args && self.cx.holds_counted(ty) {
            self.note_frozen_copy(a);
            let v = self.clone_value(v, ty);
            return self.own_value(v, ty);
        }
        v
    }

    /// May this function's code give local `l` another value (an assignment anywhere in it,
    /// also in a closure through a capture by reference, or a `BorrowMut` use)? Such a
    /// variable is no cell, but a read of a `shared` handle in it must keep the box alive.
    pub(super) fn body_changes(&mut self, l: LocalId) -> bool {
        if self.changed.is_none() {
            let changed = match self.cx.cur_def.map(|d| self.cx.hir.def(d)) {
                Some(hir::Def::Fn(f)) => changed_locals(self.cx.hir, &f.body.block),
                _ => FxHashSet::default(),
            };
            self.changed = Some(changed);
        }
        self.changed.as_ref().is_some_and(|c| c.contains(&l))
    }

    /// Is the place `e` part of what this `read` callback captured from code outside read mode
    /// (`Work::ReadOwn`: the caller's variable, never a store's object, since what a reader
    /// stores there is a copy)?
    pub(super) fn captured_root(&mut self, e: &hir::Expr) -> bool {
        if !self.own_captures {
            return false;
        }
        let Some(root) = self.own_place_root(e) else {
            return false;
        };
        let captured = match self.cx.cur_def.map(|d| self.cx.hir.def(d)) {
            Some(hir::Def::Fn(f)) => f.captures.iter().any(|c| c.inner == root),
            _ => false,
        };
        captured || self.caller_alias(root)
    }

    /// [`place_root`], not through a copy of an object type's fields (`part_copy_upcast`),
    /// which is a new object rather than a part of its operand.
    fn own_place_root(&mut self, e: &hir::Expr) -> Option<LocalId> {
        match &e.kind {
            K::Local(l, _) => Some(*l),
            K::Upcast(_) if self.part_copy_upcast(e) => None,
            K::Field { base, .. }
            | K::Index { base, .. }
            | K::UnwrapSome(base, _)
            | K::UnwrapVariant { expr: base, .. }
            | K::Downcast(base)
            | K::Upcast(base) => self.own_place_root(base),
            _ => None,
        }
    }

    /// The frozen value `e` read in place: its own use borrowed whatever its HIR mode, since
    /// moving out of one would leave the all-zero value where other threads read it, and moving
    /// out of a counted part shares it.
    pub(super) fn frozen_read(&mut self, e: &hir::Expr) -> Operand {
        let mut read = e.clone();
        match &mut read.kind {
            K::Local(_, mode)
            | K::Field { mode, .. }
            | K::Index { mode, .. }
            | K::UnwrapSome(_, mode)
            | K::UnwrapVariant { mode, .. } => *mode = UseMode::Borrow,
            _ => {}
        }
        self.expr(&read)
    }

    /// The frozen value `e` shares (`Intrinsic::Share` of a frozen root), if it is such a share.
    pub(super) fn frozen_share<'e>(&mut self, e: &'e hir::Expr) -> Option<&'e hir::Expr> {
        match &e.kind {
            K::Call {
                callee: Callee::Intrinsic(Intrinsic::Share),
                args,
            } if args.len() == 1 && self.frozen_root(&args[0]) => Some(&args[0]),
            _ => None,
        }
    }

    /// Is `local` the hidden local a map loop holds its map in (sema for_map.rs names it
    /// `<map@N>`), which only the loop's own cursor calls read?
    pub(super) fn map_hold(&self, local: hir::LocalId) -> bool {
        let Some(def) = self.cx.cur_def else {
            return false;
        };
        match self.cx.hir.def(def) {
            hir::Def::Fn(f) => f
                .body
                .locals
                .get(local.0 as usize)
                .is_some_and(|l| l.name.starts_with("<map@")),
            _ => false,
        }
    }

    /// The closure literals among a frozen call's `args` (after `skip` lowered leading params)
    /// passed to borrowed params ([`KnownFn`]).
    pub(super) fn known_fns_of(
        &mut self,
        modes: &[PassMode],
        args: &[hir::Expr],
        skip: usize,
    ) -> Vec<KnownFn> {
        let mut out = vec![];
        for (i, (a, m)) in args.iter().zip(modes).enumerate() {
            // A param given one passes it on (`find` calling `findIndex(f)`).
            if let (K::Local(l, _), PassMode::Borrow) = (&a.kind, m) {
                if let Some(k) = self.known_fns.get(l) {
                    out.push(KnownFn {
                        param: (i + skip) as u32,
                        ..k.clone()
                    });
                }
                continue;
            }
            let K::Closure(def) = a.kind else {
                continue;
            };
            let plain =
                matches!(self.cx.hir.def(def), hir::Def::Fn(c) if !c.is_async && !c.is_generator);
            if *m == PassMode::Borrow && plain {
                out.push(KnownFn {
                    param: (i + skip) as u32,
                    def,
                    targs: self.targs.clone(),
                    captures: self.frozen_captures(def),
                });
            }
        }
        out
    }

    /// A call of `f`, a function param given a closure literal ([`KnownFn`]), when some of
    /// `args` are frozen counted values for the closure's borrowed params: its frozen instance,
    /// called with this env and the frozen arguments borrowed.
    pub(super) fn call_known_fn(
        &mut self,
        f: &hir::Expr,
        args: &[hir::Expr],
        ty: hir::TyId,
        throws: Option<hir::TyId>,
    ) -> Option<Operand> {
        let K::Local(l, _) = f.kind else {
            return None;
        };
        let k = self.known_fns.get(&l)?.clone();
        let c = self.cx.fn_def(k.def);
        let n = c.captures.len();
        let modes: Vec<PassMode> = c.params[n..].iter().map(|p| p.mode).collect();
        let mask = self.frozen_mask(&modes, args, n);
        if mask == 0 {
            return None;
        }
        let work = Work::Frozen(k.def, k.targs, k.captures | mask, None, vec![]);
        // Called directly: in this function's read mode, as a direct call (`Work::Checked`).
        let code = self.cx.func_direct(work, self.mode);
        let ret = self.sub(ty);
        let outer = std::mem::replace(&mut self.frozen_args, true);
        let v = match n {
            // Without captures the closure's own code takes no env (its value's code is a
            // thunk that drops it, closure.rs).
            0 => {
                let (argv, params) = self.borrow_args(args, None, false, false);
                self.call_code(cfunc(code), argv, params, ret, throws)
            }
            _ => {
                let fty = self.sub(f.ty);
                let fv = self.expr(f);
                let fp = self.place_of(fv, fty);
                let env = Rvalue::Use(Operand::Copy(proj(&fp, Proj::Field(1))));
                let env = self.rvalue_temp(Ty::Ptr, env);
                self.call_ptr(cfunc(code), env, args, None, (ret, throws, false))
            }
        };
        self.frozen_args = outer;
        Some(v)
    }

    /// `let pat = <map>.__keyAt(cursor)` (or `__valueAt`, `__entryAt`) in a loop over a frozen
    /// map (sema for_map.rs; std map.vlt "live `for...of`"), whose bindings nothing changes:
    /// bound into the key and value the map stores, frozen, as a loop over an array binds its
    /// elements, instead of to the copies the methods' frozen instances return. True when done.
    pub(super) fn frozen_map_entry(&mut self, pat: &Pat, init: &hir::Expr) -> bool {
        let K::Call {
            callee: Callee::Def(def, _),
            args,
        } = &init.kind
        else {
            return false;
        };
        let [recv, cursor] = args.as_slice() else {
            return false;
        };
        let K::Local(hold, _) = recv.kind else {
            return false;
        };
        if !self.frozen.contains(&hold) || !self.map_hold(hold) {
            return false;
        }
        let hir::Def::Fn(f) = self.cx.hir.def(*def) else {
            return false;
        };
        let read = ["__keyAt", "__valueAt", "__entryAt"]
            .into_iter()
            .find(|m| f.name.ends_with(&format!("Map.{m}")));
        let Some(read) = read else {
            return false;
        };
        let mut binds = vec![];
        if !simple_bindings(pat, &mut binds) {
            return false;
        }
        if self.changed.is_none() {
            let changed = match self.cx.cur_def.map(|d| self.cx.hir.def(d)) {
                Some(hir::Def::Fn(f)) => changed_locals(self.cx.hir, &f.body.block),
                _ => return false,
            };
            self.changed = Some(changed);
        }
        let changed = self.changed.as_ref().expect("computed above");
        if binds.iter().any(|b| changed.contains(b)) {
            return false;
        }
        let mty = self.sub(recv.ty);
        let (Some(keys), Some(vals)) = (
            self.field_named(mty, "entryKeys"),
            self.field_named(mty, "entryValues"),
        ) else {
            return false;
        };
        let cty = self.sub(cursor.ty);
        let Some(at) = self.field_named(cty, "at") else {
            return false;
        };
        let span = init.span;
        let mk = |kind, ty| hir::Expr { kind, ty, span };
        let usize_ty = self.cx.member_ty(cty, at);
        let at = mk(
            K::Field {
                base: Box::new(cursor.clone()),
                index: at,
                mode: UseMode::Copy,
            },
            usize_ty,
        );
        let slot = |lw: &mut Self, field: u32| {
            let aty = lw.cx.member_ty(mty, field);
            let ety = lw.elem_ty(aty);
            let arr = mk(
                K::Field {
                    base: Box::new(recv.clone()),
                    index: field,
                    mode: UseMode::Borrow,
                },
                aty,
            );
            mk(
                K::Index {
                    base: Box::new(arr),
                    index: Box::new(at.clone()),
                    mode: UseMode::Borrow,
                },
                ety,
            )
        };
        let key = slot(self, keys);
        let value = slot(self, vals);
        // The value slot is `V | null`; a live entry's holds a `V` (`__advance` skipped the
        // others, and a `V` that holds null itself is the slot's value as it is).
        let value = match self.cx.kind(value.ty) {
            hir::TyKind::Option(vt, _) => mk(K::UnwrapSome(Box::new(value), UseMode::Borrow), vt),
            _ => value,
        };
        let ty = self.sub(init.ty);
        self.frozen.extend(binds.iter().copied());
        let place = match read {
            "__keyAt" => {
                let v = self.frozen_read(&key);
                self.place_of(v, key.ty)
            }
            "__valueAt" => {
                let v = self.frozen_read(&value);
                self.place_of(v, value.ty)
            }
            _ => {
                let k = self.frozen_read(&key);
                let v = self.frozen_read(&value);
                let Ty::Agg(a) = self.cx.ty(ty) else {
                    return false;
                };
                let t = self.temp(Ty::Agg(a));
                self.assign(Place::local(t), Rvalue::Aggregate(a, vec![k, v]));
                Place::local(t)
            }
        };
        if !self.dead() {
            self.bind_frozen(true, pat, &place, ty);
        }
        true
    }

    /// The index of the field named `name` of the class or struct `ty`.
    fn field_named(&mut self, ty: hir::TyId, name: &str) -> Option<u32> {
        let hir::TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return None;
        };
        let a = self.cx.adt_def(d);
        a.fields
            .iter()
            .position(|f| f.name == name)
            .map(|i| i as u32)
    }

    /// The locals `pat` binds by reference become frozen (bound into a frozen value).
    pub(super) fn freeze_bindings(&mut self, pat: &Pat) {
        match &pat.kind {
            PatKind::Binding(id, UseMode::Borrow | UseMode::BorrowMut) => {
                self.frozen.insert(*id);
            }
            PatKind::Variant { args: ps, .. } | PatKind::Tuple(ps) | PatKind::Or(ps) => {
                ps.iter().for_each(|p| self.freeze_bindings(p))
            }
            PatKind::Adt { fields } => fields.iter().for_each(|(_, p)| self.freeze_bindings(p)),
            PatKind::Array { elems, .. } => elems.iter().for_each(|p| self.freeze_bindings(p)),
            PatKind::Some(p) => self.freeze_bindings(p),
            _ => {}
        }
    }
}

/// Does `pat` only bind whole values (a binding, or a tuple of bindings and wildcards)? Its
/// bindings go to `out`.
fn simple_bindings(pat: &Pat, out: &mut Vec<LocalId>) -> bool {
    match &pat.kind {
        PatKind::Binding(id, _) => {
            out.push(*id);
            true
        }
        PatKind::Wildcard => true,
        PatKind::Tuple(ps) => ps.iter().all(|p| simple_bindings(p, out)),
        _ => false,
    }
}

/// The locals of `body` that some code may change or change something through: assigned (or
/// the root of an assigned place), used mutably (`BorrowMut`, also as the root of a projection
/// used so), or captured mutably by a closure.
pub(super) fn changed_locals(hir: &hir::Program, body: &hir::Block) -> FxHashSet<LocalId> {
    let mut c = Changed {
        hir,
        out: FxHashSet::default(),
    };
    c.block(body);
    c.out
}

struct Changed<'h> {
    hir: &'h hir::Program,
    out: FxHashSet<LocalId>,
}

/// The local a place is rooted in (through a copy of an object type's fields too, which
/// its users here only over-approximate: a write to it, or an alias of it).
pub(super) fn place_root(e: &hir::Expr) -> Option<LocalId> {
    match &e.kind {
        K::Local(l, _) => Some(*l),
        K::Field { base, .. }
        | K::Index { base, .. }
        | K::UnwrapSome(base, _)
        | K::UnwrapVariant { expr: base, .. }
        | K::Downcast(base)
        | K::Upcast(base) => place_root(base),
        _ => None,
    }
}

impl Changed<'_> {
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
            K::Closure(d) => {
                if let hir::Def::Fn(c) = self.hir.def(*d) {
                    let changed = c.captures.iter().filter(|c| c.mode == PassMode::BorrowMut);
                    self.out.extend(changed.map(|c| c.outer));
                }
            }
            K::Assign { place, .. } | K::CompoundAssign { place, .. } => {
                self.out.extend(place_root(place));
            }
            K::Local(l, UseMode::BorrowMut) => {
                self.out.insert(*l);
            }
            K::Field {
                mode: UseMode::BorrowMut,
                ..
            }
            | K::Index {
                mode: UseMode::BorrowMut,
                ..
            }
            | K::UnwrapSome(_, UseMode::BorrowMut)
            | K::UnwrapVariant {
                mode: UseMode::BorrowMut,
                ..
            } => self.out.extend(place_root(e)),
            _ => {}
        }
        match &e.kind {
            K::Block(b) => self.block(b),
            _ => super::frame_envs::children(e, &mut |x| self.expr(x)),
        }
    }
}
