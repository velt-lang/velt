//! Operands read before the operands after them run (JS order). An operand is lowered to a lazy
//! read of a place where it can; when a later sibling may change that place, the operand is
//! *held* first: a value that owns nothing is copied, anything else gets a reference of its own
//! (a string, an object, a `T | null`, a tuple, a function value), so a later operand that
//! reassigns a local (`f(i, i++)`), reallocates an array or replaces an element
//! (`console.log(ss[0], grow(ss))`, #580) leaves the earlier value as it was.

use velt_sema::effects::may_change_memory;
use velt_sema::hir::{self, DefId, PassMode, TyId};

use super::FnLower;
use crate::vir::{Operand, Place, Rvalue, Ty};

/// What the operands after one operand may do.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(super) struct Later {
    /// Write to a local of the enclosing function (`i++`, a closure called through a value).
    pub(super) locals: bool,
    /// With `locals`: the one local they write, when it is known (`dv.getUint8(q++)` writes
    /// only `q`, so `dv` needs no hold).
    pub(super) only: Option<hir::LocalId>,
    /// Change memory: a call, an assignment, `new`, `await` (`velt_sema::effects`).
    pub(super) memory: bool,
}

impl Later {
    /// What evaluating `e` may do, taking every closure it creates to write locals.
    pub(super) fn of(e: &hir::Expr) -> Later {
        Later::of_in(e, &|_| Written::Unknown)
    }

    /// What evaluating `e` may do, where `writes(d)` tells which locals of the enclosing function
    /// the closure `d` may assign when it runs ([`closure_writes`]).
    fn of_in(e: &hir::Expr, writes: &dyn Fn(DefId) -> Written) -> Later {
        let only = match written_local(e, writes) {
            Written::One(l) => Some(l),
            _ => None,
        };
        Later {
            locals: writes_local(e, writes),
            only,
            memory: may_change_memory(e),
        }
    }

    /// Does it do anything an earlier operand must be held against?
    pub(super) fn any(self) -> bool {
        self.locals || self.memory
    }

    /// Both effects.
    pub(super) fn or(self, o: Later) -> Later {
        let only = match (self.locals, o.locals) {
            (true, true) if self.only == o.only => self.only,
            (true, true) => None,
            (true, false) => self.only,
            (false, _) => o.only,
        };
        Later {
            locals: self.locals || o.locals,
            only,
            memory: self.memory || o.memory,
        }
    }

    /// May the operands write the HIR local `l`?
    pub(super) fn may_write(self, l: hir::LocalId) -> bool {
        self.locals && self.only.is_none_or(|o| o == l)
    }
}

/// For each of `es`, what the ones after it may do, where `writes` tells what each closure may
/// write ([`Later::of_in`]): one pass from the end, so a long literal (a table of 50k entries)
/// costs one look at each entry.
fn later_each_in(es: &[hir::Expr], writes: &dyn Fn(DefId) -> Written) -> Vec<Later> {
    let mut out = vec![Later::default(); es.len()];
    let mut acc = Later::default();
    for (i, e) in es.iter().enumerate().rev() {
        out[i] = acc;
        acc = acc.or(Later::of_in(e, writes));
    }
    out
}

/// The local a read of `e` starts from (`s`, `o.name`, `xs[i]`), if it is one.
pub(super) fn root_local(e: &hir::Expr) -> Option<hir::LocalId> {
    use hir::ExprKind as K;
    match &e.kind {
        K::Local(l, _) => Some(*l),
        K::Field { base, .. } | K::Index { base, .. } => root_local(base),
        K::Cast(x)
        | K::Upcast(x)
        | K::Downcast(x)
        | K::UnwrapSome(x, _)
        | K::UnwrapVariant { expr: x, .. } => root_local(x),
        _ => None,
    }
}

/// Can evaluating `e` assign local `l` in this function's own code (`l = …`, `l += …`)? A
/// call is followed into its arguments only, as no callee assigns a variable of the caller
/// that is not in a shared cell. This is weaker than [`written_local`], which takes any call
/// that may run a closure as writing locals (`writes_local`: a closure called through a value
/// may write those in cells); `changed_hold` asks it only of a variable outside a cell, where
/// a call through a function value (`hooks[0](acc, nums[0]())`) cannot assign it.
/// Conservative: shapes not listed may.
pub(super) fn assigns_local(e: &hir::Expr, l: hir::LocalId) -> bool {
    use hir::ExprKind as K;
    match &e.kind {
        K::Lit(_) | K::Global(_) | K::FnRef(..) | K::Local(..) | K::Closure(_) => false,
        K::Assign { place, value } | K::CompoundAssign { place, value, .. } => {
            root_local(place) == Some(l) || assigns_local(place, l) || assigns_local(value, l)
        }
        // `i++` is `{ let t = i; i += 1; t }`.
        K::Block(b) => {
            b.stmts.iter().any(|st| match &st.kind {
                hir::StmtKind::Let { init, .. } => {
                    init.as_ref().is_some_and(|x| assigns_local(x, l))
                }
                hir::StmtKind::Expr(x) => assigns_local(x, l),
                _ => true,
            }) || b.value.as_deref().is_some_and(|x| assigns_local(x, l))
        }
        _ => any_operand(e, &mut |x| assigns_local(x, l)).unwrap_or(true),
    }
}

/// Does `f` hold for an operand of `e`, for the shapes whose operands are all the evaluation
/// does ([`may_move_local`](FnLower::may_move_local), [`assigns_local`]): unary and binary
/// operators, casts and wraps, field and index reads, conditionals, array literals and calls
/// (their arguments, and the function value of an indirect call)? `None` for any other shape.
pub(super) fn any_operand(e: &hir::Expr, f: &mut dyn FnMut(&hir::Expr) -> bool) -> Option<bool> {
    use hir::ExprKind as K;
    Some(match &e.kind {
        K::Unary { expr: x, .. }
        | K::Cast(x)
        | K::Upcast(x)
        | K::Downcast(x)
        | K::WrapSome(x)
        | K::UnwrapSome(x, _)
        | K::UnwrapVariant { expr: x, .. }
        | K::Field { base: x, .. } => f(x),
        K::Index { base, index, .. } => f(base) || f(index),
        K::Binary { lhs, rhs, .. } | K::Logical { lhs, rhs, .. } => f(lhs) || f(rhs),
        K::If { cond, then, els } => f(cond) || f(then) || f(els),
        K::Call { callee, args } => {
            args.iter().any(&mut *f) || matches!(callee, hir::Callee::Indirect(c) if f(c))
        }
        K::ArrayLit(xs) => xs.iter().any(f),
        K::ConstArray(c) => c.elem_exprs(e.span.file).any(|x| f(&x)),
        _ => return None,
    })
}

/// The locals evaluating `e` may write, as far as `written_local` tells them apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Written {
    Nothing,
    One(hir::LocalId),
    Unknown,
}

impl Written {
    fn and(self, o: Written) -> Written {
        match (self, o) {
            (Written::Nothing, x) | (x, Written::Nothing) => x,
            (Written::One(a), Written::One(b)) if a == b => Written::One(a),
            _ => Written::Unknown,
        }
    }
}

/// The local evaluating `e` writes, when it writes just one and the shape says which: an
/// assignment or update of a variable (`q++`, which is `{ let t = q; q += 1; t }`: the block's
/// own temporaries are not visible to anything evaluated before it), or a closure that assigns
/// one variable it captures, passed to a direct call or put in a literal (`run(() => k++)`),
/// whose other parts write nothing else. `Unknown` for anything else that may write.
fn written_local(e: &hir::Expr, writes: &dyn Fn(DefId) -> Written) -> Written {
    use hir::ExprKind as K;
    let w = |x: &hir::Expr| written_local(x, writes);
    let all = |xs: &[hir::Expr]| xs.iter().fold(Written::Nothing, |a, x| a.and(w(x)));
    match &e.kind {
        K::Assign { place, value } | K::CompoundAssign { place, value, .. } => match &place.kind {
            K::Local(l, _) => Written::One(*l).and(w(value)),
            _ => Written::Unknown,
        },
        K::Block(b) => {
            let mut acc = Written::Nothing;
            let mut own = vec![];
            for st in &b.stmts {
                let x = match &st.kind {
                    hir::StmtKind::Let { local, init } => {
                        own.push(*local);
                        init.as_ref().map_or(Written::Nothing, w)
                    }
                    hir::StmtKind::Expr(x) => w(x),
                    _ => Written::Unknown,
                };
                acc = acc.and(match x {
                    Written::One(l) if own.contains(&l) => Written::Nothing,
                    x => x,
                });
            }
            let v = b.value.as_deref().map_or(Written::Nothing, w);
            acc.and(match v {
                Written::One(l) if own.contains(&l) => Written::Nothing,
                x => x,
            })
        }
        K::Call {
            callee: hir::Callee::Intrinsic(_) | hir::Callee::Def(..),
            args,
        }
        | K::New { args, .. }
        | K::AdtLit { fields: args, .. }
        | K::Variant { args, .. }
        | K::Tuple(args)
        | K::ArrayLit(args) => all(args),
        K::Closure(d) => writes(*d),
        _ if !writes_local(e, writes) => Written::Nothing,
        _ => Written::Unknown,
    }
}

/// Can evaluating `e` write to a local of the enclosing function? A direct call, a `new` and a
/// literal write only what their parts write: no callee assigns a variable of the caller that is
/// not in a shared cell, except through a closure it is passed. Creating a closure writes nothing,
/// but a callee may run it: `writes(d)` tells which variables the closure `d` assigns.
fn writes_local(e: &hir::Expr, writes: &dyn Fn(DefId) -> Written) -> bool {
    use hir::ExprKind as K;
    let w = |x: &hir::Expr| writes_local(x, writes);
    if let Some(wrap) = velt_sema::effects::routed_wrap(e) {
        return w(wrap);
    }
    match &e.kind {
        K::Lit(_) | K::Local(..) | K::Global(_) | K::FnRef(..) => false,
        K::Unary { expr, .. }
        | K::Cast(expr)
        | K::Upcast(expr)
        | K::Downcast(expr)
        | K::WrapSome(expr)
        | K::UnwrapSome(expr, _)
        | K::UnwrapVariant { expr, .. } => w(expr),
        K::Field { base, .. } => w(base),
        K::Index { base, index, .. } => w(base) || w(index),
        K::Binary { lhs, rhs, .. } | K::Logical { lhs, rhs, .. } => w(lhs) || w(rhs),
        K::Call { callee, args } => {
            !matches!(callee, hir::Callee::Intrinsic(_) | hir::Callee::Def(..))
                || args.iter().any(w)
        }
        K::New { args, .. }
        | K::AdtLit { fields: args, .. }
        | K::Variant { args, .. }
        | K::Tuple(args)
        | K::ArrayLit(args) => args.iter().any(w),
        K::Closure(d) => writes(*d) != Written::Nothing,
        K::If { cond, then, els } => w(cond) || w(then) || w(els),
        K::ConstArray(c) => c.elem_exprs(e.span.file).any(|x| w(&x)),
        _ => true,
    }
}

/// The variables of the function that created the closure `f` it may assign outside a cell when it
/// runs: those it captures by mutable borrow. A variable in a cell is read through the cell, a
/// place a later operand that may change memory always holds the value of (`hold`).
fn closure_writes(f: &hir::FnDef) -> Written {
    (f.captures.iter())
        .filter(|c| c.mode == PassMode::BorrowMut)
        .fold(Written::Nothing, |w, c| w.and(Written::One(c.outer)))
}

impl FnLower<'_, '_> {
    /// Can evaluating `e` move local `l` (and so free what it holds)? A read moved out of `l`
    /// moves `l`: a narrowed `o: A2 | null` goes to a call as `UnwrapSome(o)` marked as the move.
    /// A constructor call, a literal or a variant moves what its arguments move (`new R()`,
    /// `{ id: 1 }` move nothing of the caller's), a `match` what its scrutinee, guards and arms
    /// move, a closure the variables it captures by move or in a cell. Conservative: shapes not
    /// listed may.
    pub(super) fn may_move_local(&self, e: &hir::Expr, l: hir::LocalId) -> bool {
        use hir::ExprKind as K;
        use hir::UseMode::Move;
        match &e.kind {
            K::Lit(_) | K::Global(_) | K::FnRef(..) => false,
            K::Local(x, mode) => *x == l && *mode == Move,
            K::UnwrapSome(x, Move)
            | K::UnwrapVariant {
                expr: x,
                mode: Move,
                ..
            }
            | K::Field {
                base: x,
                mode: Move,
                ..
            }
            | K::Index {
                base: x,
                mode: Move,
                ..
            } if root_local(x) == Some(l) => true,
            K::New { args, .. }
            | K::AdtLit { fields: args, .. }
            | K::Variant { args, .. }
            | K::Tuple(args) => args.iter().any(|x| self.may_move_local(x, l)),
            K::ToDyn { expr: x, .. } => self.may_move_local(x, l),
            // `e.over!`: what the arms bind is read from the scrutinee, moved only if it is.
            K::Match { scrutinee, arms } => {
                self.may_move_local(scrutinee, l)
                    || arms.iter().any(|a| {
                        a.guard.as_ref().is_some_and(|g| self.may_move_local(g, l))
                            || self.may_move_local(&a.body, l)
                    })
            }
            K::Throw(x) => self.may_move_local(x, l),
            K::Closure(d) => {
                let f = self.cx.fn_def(*d);
                f.captures.iter().any(|c| {
                    c.outer == l
                        && (c.mode == hir::PassMode::Owned && !c.share
                            || f.body.locals[c.inner.0 as usize].boxed)
                })
            }
            _ => any_operand(e, &mut |x| self.may_move_local(x, l)).unwrap_or(true),
        }
    }

    /// [`Later::of`] in this program: a field read on a proxiable type (#823, proxy_typed.rs)
    /// may run a `get` trap, user code that may change any memory, like an `any` member read;
    /// an element read of an array view type (array_view.rs) may convert the element into the
    /// view's mirror, which moves the mirror's buffer and so the elements read before it
    /// (#1942).
    pub(super) fn later_of(&mut self, e: &hir::Expr) -> Later {
        let cx = &self.cx;
        let mut l = Later::of_in(e, &|d| closure_writes(cx.fn_def(d)));
        if !l.memory && self.read_runs_code(e) {
            l.memory = true;
        }
        l
    }

    /// What the operands after each of `es` may do, with [`later_of`](Self::later_of).
    pub(super) fn later_each(&mut self, es: &[hir::Expr]) -> Vec<Later> {
        let cx = &self.cx;
        let mut out = later_each_in(es, &|d| closure_writes(cx.fn_def(d)));
        if self.reads_may_run_code() {
            let mut acc = false;
            for (i, e) in es.iter().enumerate().rev() {
                out[i].memory |= acc;
                acc = acc || self.read_runs_code(e);
            }
        }
        out
    }

    /// Can evaluating `e` change what an earlier operand reads?
    pub(super) fn may_write(&mut self, e: &hir::Expr) -> bool {
        self.later_of(e).any()
    }

    /// [`may_change_memory`] with [`later_of`](Self::later_of).
    pub(super) fn may_change_memory(&mut self, e: &hir::Expr) -> bool {
        self.later_of(e).memory
    }

    /// May a read in this program run code ([`Self::read_runs_code`])?
    pub(super) fn reads_may_run_code(&self) -> bool {
        self.cx.has_proxies() || !self.cx.boxing.array_views.is_empty()
    }

    /// Does evaluating `e` read a field of a proxiable type, or an element of an array view
    /// type ([`Self::later_of`])? Only the shapes `may_change_memory` looks into: it answers
    /// yes for the others already.
    fn read_runs_code(&mut self, e: &hir::Expr) -> bool {
        if !self.reads_may_run_code() {
            return false;
        }
        self.reads_proxy_or_view(e)
    }

    fn reads_proxy_or_view(&mut self, e: &hir::Expr) -> bool {
        use hir::ExprKind as K;
        match &e.kind {
            K::Field { base, .. } => {
                let bt = self.sub(base.ty);
                self.cx.proxiable(bt) || self.reads_proxy_or_view(base)
            }
            K::Index { base, index, .. } => {
                let bt = self.sub(base.ty);
                self.cx.array_view(bt)
                    || self.reads_proxy_or_view(base)
                    || self.reads_proxy_or_view(index)
            }
            K::Unary { expr: x, .. }
            | K::Cast(x)
            | K::WrapSome(x)
            | K::Upcast(x)
            | K::Downcast(x)
            | K::UnwrapSome(x, _)
            | K::UnwrapVariant { expr: x, .. } => self.reads_proxy_or_view(x),
            K::Binary { lhs: a, rhs: b, .. } | K::Logical { lhs: a, rhs: b, .. } => {
                self.reads_proxy_or_view(a) || self.reads_proxy_or_view(b)
            }
            K::If { cond, then, els } => {
                self.reads_proxy_or_view(cond)
                    || self.reads_proxy_or_view(then)
                    || self.reads_proxy_or_view(els)
            }
            K::Call {
                callee: hir::Callee::Intrinsic(_),
                args,
            } => args.iter().any(|a| self.reads_proxy_or_view(a)),
            _ => false,
        }
    }

    /// Hold the borrowed operand `op` (of type `ty`) against the operands after it (`later`):
    /// a value that owns nothing is copied, except one holding an `Atomic`, which is used
    /// where it is; anything else gets a reference of its own in an
    /// owned temporary (a share), so the parts it points at outlive what the later operands
    /// free. A share that would count a type not counted yet is replaced by a bitwise copy:
    /// holding never changes the program's counted types.
    pub(super) fn hold(&mut self, op: Operand, ty: TyId, later: Later) -> Operand {
        self.hold_as(op, ty, later, false)
    }

    /// Evaluate the operand `e` and [`hold`](Self::hold) it against the operands after it. When
    /// those may change memory, the path to `e`'s value is recorded first, as `stable_borrow`
    /// records a call argument's: a value reached through a counted container or a shared cell
    /// can be freed by a later operand through another reference (`console.log(ps[0], clear())`
    /// where `clear` pops `ps` through a closure), so its type is counted and the hold shares it.
    pub(super) fn expr_held(&mut self, e: &hir::Expr, later: Later) -> Operand {
        // A frozen value is changed by nothing (frozen.rs), and its counts must not change. A
        // number read out of one is still read now (copied, which counts nothing): a later
        // operand may free the box it is in.
        if self.frozen_root(e) {
            let v = self.expr(e);
            return match super::place::number(self.vty(e.ty)) {
                true => self.hold(v, e.ty, later),
                false => v,
            };
        }
        if later.memory && !self.dead() {
            let ty = self.sub(e.ty);
            if self.cx.needs_drop(ty) && !self.through_counted(e, ty) {
                self.in_shared_cell(e);
            }
        }
        let v = self.expr(e);
        self.hold(v, e.ty, later)
    }

    /// [`hold`](Self::hold) for an operand the consumer owns (from `consume`): it is already
    /// a share of its own, so a value still in a place is only copied out of it.
    pub(super) fn hold_owned(&mut self, op: Operand, ty: TyId, later: Later) -> Operand {
        self.hold_as(op, ty, later, true)
    }

    fn hold_as(&mut self, op: Operand, ty: TyId, later: Later, owned: bool) -> Operand {
        let Operand::Copy(p) = &op else { return op };
        if !later.any() || self.dead() {
            return op;
        }
        // An element borrowed where it is while its array has one owner (stabilize.rs
        // `unique_array_elem`): its pointer is set once every argument is evaluated, where it is
        // shared if the array has another owner by then, and the later arguments run no code.
        if self.pending_borrow(p) {
            return op;
        }
        let t = self.vty(ty);
        if t == Ty::Unit {
            return op;
        }
        if !self.changeable(p, later) {
            // A fresh temporary, or a variable nothing after it assigns: only numbers keep
            // the snapshot they always had (a register copy).
            return match t.is_scalar() {
                true => self.rvalue_temp(t, Rvalue::Use(op)),
                false => op,
            };
        }
        let cty = self.sub(ty);
        // A value holding an `Atomic` stays where it is: a copy's atomics would take the
        // changes (a method on a copy of the receiver), and no other code sees them.
        let mut seen = rustc_hash::FxHashSet::default();
        if !owned && self.cx.holds_atomic(cty, &mut seen) {
            return op;
        }
        if owned || !self.cx.needs_drop(cty) || !self.cx.shares_as_counted(cty) {
            // Owned already, owning nothing, or holding an uncounted object: a later operand
            // reaches such an object only through a counted container or a shared cell, and
            // either makes its type counted, so the bitwise copy (the same object) stays valid.
            // A deep copy would detach a receiver from its object (`m.set(k, m.get(k)! + 1)`).
            let tmp = self.copy_to_temp(op, t);
            return Operand::Copy(Place::local(tmp));
        }
        let held = self.share_value(op.clone(), cty);
        match &held {
            // The fresh temporary the share was written to: registered where it is.
            Operand::Copy(h) if h.proj.is_empty() && held != op => {
                self.own_temp(h.local, cty);
                held
            }
            _ => self.own_value(held, cty),
        }
    }

    /// What borrowed argument `args[i]`, which the callee may change (a modified param of a
    /// function used as a value, or any param through a function value), is held against:
    /// `None` when it is passed where it is. Such a callee's modified params are `Borrow`, so
    /// the exclusivity rule (velt_sema `ownership/exclusive`) does not see it change what
    /// another argument reaches. An object stored inline (an uncounted object type, array or
    /// tuple, or one of them or `null`) is passed where it is, as holding it would copy it and
    /// the change would go to the copy, unless a later operand may replace it (a call through a
    /// function value that replaces the field passed first: `pushes[0](p.log, swaps[0](p))`).
    /// Its type is then counted, so the hold is a share of the one object, as in JavaScript.
    /// Counting changes the type for the whole program, so only an operand that can reach the
    /// object counts ([`reaches_objects`](Self::reaches_objects)), and never for a variable of
    /// this function that nothing after it assigns: no operand reaches its slot.
    pub(super) fn changed_hold(
        &mut self,
        args: &[hir::Expr],
        i: usize,
        later: Later,
    ) -> Option<Later> {
        let Some(obj) = self.inline_object(args[i].ty) else {
            return Some(later);
        };
        if !later.memory || self.own_variable(&args[i], &args[i + 1..]) {
            return None;
        }
        if !args[i + 1..].iter().any(|e| self.reaches_objects(e)) {
            return None;
        }
        self.cx.note_identity_borrow(obj);
        Some(later)
    }

    /// Is `e` a variable whose value this function holds itself (not a param or binding that
    /// points elsewhere, nor one in a shared cell a closure may assign) that none of `later`
    /// assigns? Only this function's code can assign such a variable: a closure that stays in
    /// the function borrows it (sema reports a call that borrows it while such a closure
    /// assigns it), and one that may be stored or called through a value keeps it in a cell.
    /// So a call through a function value (`hooks[0](acc, nums[0]())`) cannot replace it.
    fn own_variable(&self, e: &hir::Expr, later: &[hir::Expr]) -> bool {
        use hir::ExprKind as K;
        let mut e = e;
        while let K::Cast(x) | K::Upcast(x) | K::Downcast(x) = &e.kind {
            e = x;
        }
        let K::Local(l, _) = e.kind else {
            return false;
        };
        let info = &self.info[l.0 as usize];
        !info.indirect && !info.cell && !info.in_cell && !later.iter().any(|x| assigns_local(x, l))
    }

    /// Can evaluating `e` change an object stored inline that an earlier operand passes? As
    /// [`may_change_memory`](Self::may_change_memory), except that a call of a
    /// standard-library function whose arguments are all numbers, booleans or strings
    /// (`Math.sqrt(4)`, `x.toFixed(2)`, `String(n)`) reaches no such object: it is handed no
    /// reference to one, and the library sees none of the program's variables.
    pub(super) fn reaches_objects(&mut self, e: &hir::Expr) -> bool {
        use hir::ExprKind as K;
        if !self.may_change_memory(e) {
            return false;
        }
        match &e.kind {
            K::Call {
                callee: hir::Callee::Def(h, _),
                args,
            } if self.std_fn(*h) && args.iter().all(|a| self.plain_arg(a)) => {
                args.iter().any(|a| self.reaches_objects(a))
            }
            K::Unary { expr: x, .. } | K::Cast(x) | K::Upcast(x) | K::WrapSome(x) => {
                self.reaches_objects(x)
            }
            K::Binary { lhs, rhs, .. } | K::Logical { lhs, rhs, .. } => {
                self.reaches_objects(lhs) || self.reaches_objects(rhs)
            }
            K::If { cond, then, els } => {
                self.reaches_objects(cond)
                    || self.reaches_objects(then)
                    || self.reaches_objects(els)
            }
            // An intrinsic that runs no user code: its arguments (`String(n)`, `xs.length`).
            K::Call {
                callee: hir::Callee::Intrinsic(i),
                args,
            } if !velt_sema::effects::runs_user_code(*i) => args
                .iter()
                .any(|a| velt_sema::effects::writes_place(a) || self.reaches_objects(a)),
            // A new array (`Math.max(1, 2)`'s rest param, `[1, 2].length`): its elements.
            K::ArrayLit(es) => es.iter().any(|x| self.reaches_objects(x)),
            K::ConstArray(c) => c.elem_exprs(e.span.file).any(|x| self.reaches_objects(&x)),
            K::Field { base, .. } => self.read_runs_code(e) || self.reaches_objects(base),
            _ => true,
        }
    }

    /// Is `h` a function of the standard library?
    fn std_fn(&self, h: hir::DefId) -> bool {
        match (self.cx.hir.def(h), self.cx.locs.as_ref()) {
            (hir::Def::Fn(f), Some(m)) => m.is_std(f.span),
            _ => false,
        }
    }

    /// Is argument `a` a number, a boolean or a string, or a new array of them (a rest
    /// param: `Math.max(1, 2)`)? It holds no reference to the program's objects.
    fn plain_arg(&mut self, a: &hir::Expr) -> bool {
        match &a.kind {
            hir::ExprKind::ArrayLit(es) => es.iter().all(|x| self.plain_arg(x)),
            hir::ExprKind::ConstArray(c) => c.elem_exprs(a.span.file).all(|x| self.plain_arg(&x)),
            _ => self.plain_value(a.ty),
        }
    }

    /// Is a value of type `ty` a number, a boolean or a string (it holds no reference)?
    fn plain_value(&mut self, ty: TyId) -> bool {
        matches!(
            self.kind(ty),
            hir::TyKind::Int(_) | hir::TyKind::Float(_) | hir::TyKind::Bool | hir::TyKind::Str
        )
    }

    /// The object type a value of type `ty` stores inline (`changed_hold`).
    fn inline_object(&mut self, ty: TyId) -> Option<TyId> {
        let t = self.sub(ty);
        if self.cx.counted(t) || self.vty(t).is_scalar() {
            return None;
        }
        match self.cx.kind(t) {
            hir::TyKind::Option(x, _) => self.inline_object(x),
            hir::TyKind::Tuple(_) | hir::TyKind::Array(_) => Some(t),
            hir::TyKind::Adt(d, _) => matches!(
                self.cx.hir.def(d),
                hir::Def::Adt(a) if a.kind != hir::AdtKind::Class && !a.is_copy
            )
            .then_some(t),
            _ => None,
        }
    }

    /// Can what `p` holds be changed by operands that do `later`? A temporary of the lowering
    /// holds a value nobody else reaches; a variable is changed only by a local write. Anything
    /// reached through a projection (an element, a field, a variable in a cell) may change.
    fn changeable(&self, p: &Place, later: Later) -> bool {
        if !p.proj.is_empty() {
            return true;
        }
        let named = self.locals[p.local.0 as usize].name.is_some();
        if !named || !later.locals {
            return false;
        }
        match later.only {
            // The one local the later operands write: is it this one?
            Some(l) => self.info[l.0 as usize].vir == Some(p.local) && later.may_write(l),
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{later_each_in, writes_local, written_local, Later, Written};
    use velt_common::Span;
    use velt_sema::hir::{Callee, DefId, Expr, ExprKind as H, Lit, LocalId, TyId, UseMode};

    fn mk(kind: H) -> Expr {
        Expr {
            kind,
            ty: TyId(0),
            span: Span::default(),
        }
    }

    fn local() -> Expr {
        mk(H::Local(LocalId(0), UseMode::Borrow))
    }

    /// `xs[0]`.
    fn element() -> Expr {
        mk(H::Index {
            base: Box::new(local()),
            index: Box::new(mk(H::Lit(Lit::Int(0)))),
            mode: UseMode::Borrow,
        })
    }

    #[test]
    fn reads_write_no_local() {
        // `cmp(xs[b], xs[a])`: the second element read does not make the first one held.
        assert!(!writes_local(&element(), &|_| Written::Unknown));
        assert!(!writes_local(
            &mk(H::UnwrapSome(Box::new(element()), UseMode::Borrow)),
            &|_| Written::Unknown
        ));
        assert_eq!(
            later_each_in(&[element(), element()], &|_| Written::Unknown),
            vec![Later::default(); 2]
        );
    }

    #[test]
    fn a_call_through_a_value_may_write_a_local() {
        let call = mk(H::Call {
            callee: Callee::Indirect(Box::new(local())),
            args: vec![],
        });
        assert!(writes_local(&call, &|_| Written::Unknown));
        assert!(Later::of(&call).any());
    }

    #[test]
    fn a_new_writes_what_its_closures_may() {
        // `new Handler(() => other.n)`: the constructor may run the closure, which writes a
        // local only if it assigns one it captures.
        let new = mk(H::New {
            def: DefId(0),
            type_args: vec![],
            args: vec![mk(H::Closure(DefId(1)))],
        });
        assert!(!writes_local(&new, &|_| Written::Nothing));
        assert!(writes_local(&new, &|_| Written::Unknown));
        assert!(Later::of(&new).locals);
    }

    #[test]
    fn a_closure_writes_the_variable_it_assigns() {
        // `run(() => { k++; ... })`: only `k`, so a part read before it from another variable
        // needs no hold (literal_moves.rs).
        let k = LocalId(7);
        let call = mk(H::Call {
            callee: Callee::Def(DefId(0), vec![]),
            args: vec![mk(H::Closure(DefId(1)))],
        });
        let one = |_: DefId| Written::One(k);
        let later = Later::of_in(&call, &one);
        assert!(later.locals && later.may_write(k) && !later.may_write(LocalId(0)));
        let two = mk(H::Tuple(vec![call.clone(), mk(H::Closure(DefId(2)))]));
        let each = |d: DefId| Written::One(LocalId(d.0));
        assert_eq!(written_local(&two, &each), Written::Unknown);
    }
}
