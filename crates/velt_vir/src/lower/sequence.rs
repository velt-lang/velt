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

    /// Both effects.
    pub(super) fn or(self, o: Later) -> Later {
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
    /// Hold the borrowed operand `op` (of type `ty`) against the operands after it (`later`):
    /// a value that owns nothing is copied; anything else gets a reference of its own in an
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
