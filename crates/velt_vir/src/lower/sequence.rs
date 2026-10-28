//! Operands read before the operands after them run (JS order). An operand is lowered to a lazy
//! read of a place where it can; when a later sibling may change that place, the operand is
//! *held* first: a value that owns nothing is copied, anything else gets a reference of its own
//! (a string, an object, a `T | null`, a tuple, a function value), so a later operand that
//! reassigns a local (`f(i, i++)`), reallocates an array or replaces an element
//! (`console.log(ss[0], grow(ss))`, #580) leaves the earlier value as it was.

use velt_sema::effects::may_change_memory;
use velt_sema::hir::{self, TyId};

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
    /// What evaluating `e` may do.
    pub(super) fn of(e: &hir::Expr) -> Later {
        let only = match written_local(e) {
            Written::One(l) => Some(l),
            _ => None,
        };
        Later {
            locals: writes_local(e),
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

/// For each of `es`, what the ones after it may do: one pass from the end, so a long literal
/// (a table of 50k entries) costs one look at each entry.
pub(super) fn later_each(es: &[hir::Expr]) -> Vec<Later> {
    let mut out = vec![Later::default(); es.len()];
    let mut acc = Later::default();
    for (i, e) in es.iter().enumerate().rev() {
        out[i] = acc;
        acc = acc.or(Later::of(e));
    }
    out
}

/// The local a read of `e` starts from (`s`, `o.name`, `xs[i]`), if it is one.
pub(super) fn root_local(e: &hir::Expr) -> Option<hir::LocalId> {
    use hir::ExprKind as K;
    match &e.kind {
        K::Local(l, _) => Some(*l),
        K::Field { base, .. } | K::Index { base, .. } => root_local(base),
        K::Cast(x) | K::Upcast(x) | K::Downcast(x) | K::UnwrapSome(x, _) => root_local(x),
        _ => None,
    }
}

/// Can evaluating `e` move local `l` (and so free what it holds)? Conservative: shapes not
/// listed may.
pub(super) fn may_move_local(e: &hir::Expr, l: hir::LocalId) -> bool {
    use hir::ExprKind as K;
    let any = |es: &[hir::Expr]| es.iter().any(|x| may_move_local(x, l));
    match &e.kind {
        K::Lit(_) | K::Global(_) | K::FnRef(..) => false,
        K::Local(x, mode) => *x == l && *mode == hir::UseMode::Move,
        K::Unary { expr: x, .. }
        | K::Cast(x)
        | K::Upcast(x)
        | K::Downcast(x)
        | K::WrapSome(x)
        | K::UnwrapSome(x, _)
        | K::UnwrapVariant { expr: x, .. }
        | K::Field { base: x, .. } => may_move_local(x, l),
        K::Index { base, index, .. } => may_move_local(base, l) || may_move_local(index, l),
        K::Binary { lhs, rhs, .. } | K::Logical { lhs, rhs, .. } => {
            may_move_local(lhs, l) || may_move_local(rhs, l)
        }
        K::If { cond, then, els } => {
            may_move_local(cond, l) || may_move_local(then, l) || may_move_local(els, l)
        }
        K::Call { callee, args } => {
            any(args) || matches!(callee, hir::Callee::Indirect(f) if may_move_local(f, l))
        }
        K::ArrayLit(xs) => any(xs),
        _ => true,
    }
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
/// own temporaries are not visible to anything evaluated before it), whose parts write nothing
/// else. `Unknown` for anything else that may write.
fn written_local(e: &hir::Expr) -> Written {
    use hir::ExprKind as K;
    match &e.kind {
        K::Assign { place, value } | K::CompoundAssign { place, value, .. } => match &place.kind {
            K::Local(l, _) => Written::One(*l).and(written_local(value)),
            _ => Written::Unknown,
        },
        K::Block(b) => {
            let mut w = Written::Nothing;
            let mut own = vec![];
            for st in &b.stmts {
                let x = match &st.kind {
                    hir::StmtKind::Let { local, init } => {
                        own.push(*local);
                        init.as_ref().map_or(Written::Nothing, written_local)
                    }
                    hir::StmtKind::Expr(x) => written_local(x),
                    _ => Written::Unknown,
                };
                w = w.and(match x {
                    Written::One(l) if own.contains(&l) => Written::Nothing,
                    x => x,
                });
            }
            let v = b.value.as_deref().map_or(Written::Nothing, written_local);
            w.and(match v {
                Written::One(l) if own.contains(&l) => Written::Nothing,
                x => x,
            })
        }
        _ if !writes_local(e) => Written::Nothing,
        _ => Written::Unknown,
    }
}

/// Can evaluating `e` write to a local of the enclosing function?
fn writes_local(e: &hir::Expr) -> bool {
    use hir::ExprKind as K;
    if let Some(wrap) = velt_sema::effects::routed_wrap(e) {
        return writes_local(wrap);
    }
    match &e.kind {
        K::Lit(_) | K::Local(..) | K::Global(_) | K::FnRef(..) => false,
        K::Unary { expr, .. }
        | K::Cast(expr)
        | K::Upcast(expr)
        | K::Downcast(expr)
        | K::WrapSome(expr)
        | K::UnwrapSome(expr, _)
        | K::UnwrapVariant { expr, .. } => writes_local(expr),
        K::Field { base, .. } => writes_local(base),
        K::Index { base, index, .. } => writes_local(base) || writes_local(index),
        K::Binary { lhs, rhs, .. } | K::Logical { lhs, rhs, .. } => {
            writes_local(lhs) || writes_local(rhs)
        }
        K::Call { callee, args } => {
            !matches!(callee, hir::Callee::Intrinsic(_) | hir::Callee::Def(..))
                || args.iter().any(writes_local)
        }
        K::If { cond, then, els } => writes_local(cond) || writes_local(then) || writes_local(els),
        _ => true,
    }
}

impl FnLower<'_, '_> {
    /// [`Later::of`] in this program: a field read on a proxiable type (#823, proxy_typed.rs)
    /// may run a `get` trap, user code that may change any memory, like an `any` member read.
    pub(super) fn later_of(&mut self, e: &hir::Expr) -> Later {
        let mut l = Later::of(e);
        if !l.memory && self.reads_proxy(e) {
            l.memory = true;
        }
        l
    }

    /// [`later_each`] with [`later_of`](Self::later_of).
    pub(super) fn later_each(&mut self, es: &[hir::Expr]) -> Vec<Later> {
        let mut out = later_each(es);
        if self.cx.has_proxies() {
            let mut acc = false;
            for (i, e) in es.iter().enumerate().rev() {
                out[i].memory |= acc;
                acc = acc || self.reads_proxy(e);
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

    /// Does evaluating `e` read a field of a proxiable type? Only the shapes
    /// `may_change_memory` looks into: it answers yes for the others already.
    fn reads_proxy(&mut self, e: &hir::Expr) -> bool {
        use hir::ExprKind as K;
        if !self.cx.has_proxies() {
            return false;
        }
        match &e.kind {
            K::Field { base, .. } => {
                let bt = self.sub(base.ty);
                self.cx.proxiable(bt) || self.reads_proxy(base)
            }
            K::Unary { expr: x, .. }
            | K::Cast(x)
            | K::WrapSome(x)
            | K::Upcast(x)
            | K::Downcast(x)
            | K::UnwrapSome(x, _)
            | K::UnwrapVariant { expr: x, .. } => self.reads_proxy(x),
            K::Index {
                base: a, index: b, ..
            }
            | K::Binary { lhs: a, rhs: b, .. }
            | K::Logical { lhs: a, rhs: b, .. } => self.reads_proxy(a) || self.reads_proxy(b),
            K::If { cond, then, els } => {
                self.reads_proxy(cond) || self.reads_proxy(then) || self.reads_proxy(els)
            }
            K::Call {
                callee: hir::Callee::Intrinsic(_),
                args,
            } => args.iter().any(|a| self.reads_proxy(a)),
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
    /// the change would go to the copy, unless a later operand may change memory (a call
    /// through a function value that replaces the field passed first:
    /// `pushes[0](p.log, swaps[0](p))`). Its type is then counted, so the hold is a share of
    /// the one object, as in JavaScript.
    pub(super) fn changed_hold(
        &mut self,
        args: &[hir::Expr],
        i: usize,
        later: Later,
    ) -> Option<Later> {
        let Some(obj) = self.inline_object(args[i].ty) else {
            return Some(later);
        };
        if later.memory {
            self.cx.note_identity_borrow(obj);
            return Some(later);
        }
        None
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
    use super::{later_each, writes_local, Later};
    use velt_common::Span;
    use velt_sema::hir::{Callee, Expr, ExprKind as H, Lit, LocalId, TyId, UseMode};

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
        assert!(!writes_local(&element()));
        assert!(!writes_local(&mk(H::UnwrapSome(
            Box::new(element()),
            UseMode::Borrow
        ))));
        assert_eq!(
            later_each(&[element(), element()]),
            vec![Later::default(); 2]
        );
    }

    #[test]
    fn a_call_through_a_value_may_write_a_local() {
        let call = mk(H::Call {
            callee: Callee::Indirect(Box::new(local())),
            args: vec![],
        });
        assert!(writes_local(&call));
        assert!(Later::of(&call).any());
    }
}
