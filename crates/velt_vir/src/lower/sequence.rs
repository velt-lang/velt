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
    /// Change memory: a call, an assignment, `new`, `await` (`velt_sema::effects`).
    pub(super) memory: bool,
}

impl Later {
    /// What evaluating `e` may do.
    pub(super) fn of(e: &hir::Expr) -> Later {
        Later {
            locals: writes_local(e),
            memory: may_change_memory(e),
        }
    }

    /// Does it do anything an earlier operand must be held against?
    pub(super) fn any(self) -> bool {
        self.locals || self.memory
    }

    fn or(self, o: Later) -> Later {
        Later {
            locals: self.locals || o.locals,
            memory: self.memory || o.memory,
        }
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

/// Can evaluating `e` change what an earlier operand reads?
pub(super) fn may_write(e: &hir::Expr) -> bool {
    Later::of(e).any()
}

/// Can evaluating `e` write to a local of the enclosing function?
fn writes_local(e: &hir::Expr) -> bool {
    use hir::ExprKind as K;
    match &e.kind {
        K::Lit(_) | K::Local(..) | K::Global(_) | K::FnRef(..) => false,
        K::Unary { expr, .. } | K::Cast(expr) | K::Upcast(expr) | K::Downcast(expr) => {
            writes_local(expr)
        }
        K::Field { base, .. } => writes_local(base),
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
    /// Hold the borrowed operand `op` (of type `ty`) against the operands after it (`later`):
    /// a value that owns nothing is copied; anything else gets a reference of its own in an
    /// owned temporary, so the parts it points at outlive what the later operands free. That
    /// reference is a share (`share_value`), or a deep copy (`clone_value`) where a share would
    /// count a type that is not counted yet: holding never changes the program's counted types.
    pub(super) fn hold(&mut self, op: Operand, ty: TyId, later: Later) -> Operand {
        self.hold_as(op, ty, later, false)
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
        if owned || !self.cx.needs_drop(cty) {
            let tmp = self.copy_to_temp(op, t);
            return Operand::Copy(Place::local(tmp));
        }
        let held = match self.cx.shares_as_counted(cty) {
            true => self.share_value(op, cty),
            false => self.clone_value(op, cty),
        };
        self.own_value(held, cty)
    }

    /// Can what `p` holds be changed by operands that do `later`? A temporary of the lowering
    /// holds a value nobody else reaches; a variable is changed only by a local write. Anything
    /// reached through a projection (an element, a field, a variable in a cell) may change.
    fn changeable(&self, p: &Place, later: Later) -> bool {
        if !p.proj.is_empty() {
            return true;
        }
        let named = self.locals[p.local.0 as usize].name.is_some();
        named && later.locals
    }
}
