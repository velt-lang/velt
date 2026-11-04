//! Moves out of a local that a later operand of the same literal still reads (#2258).
//!
//! The move analysis (`velt_sema::moves`, `operands`) checks the operands of a literal (object,
//! array, tuple, variant) as JavaScript evaluates them, with each moved place moved only once
//! all of them are evaluated: `{ title: p.name, body: show(p) }` reads `p` whole after
//! `p.name` is taken. Most moves leave the moved bits in the local (`moved_fields`), so the
//! later read sees them. But a move out of a part of a local that stays owned (the payload of
//! a narrowed `p: T | null`, a field of a local with a drop flag) leaves the all-zero value
//! behind at once (`take_part`), and a move out of a narrowed union member drops the rest of
//! the member (`move_member_field`): the later read would see an empty string, or freed memory.
//! Such an operand is read in place and shared instead (a reference of its own) when it is a
//! string: the local keeps its part and drops it as usual. Otherwise it is lowered after the
//! operands that follow it, the order the move analysis checks, unless one of them may write
//! the local.

use rustc_hash::FxHashSet;
use velt_sema::hir::{self, LocalId, UseMode};

use super::place::part_root;
use super::sequence::any_operand;
use super::{unit, FnLower};
use crate::vir::Operand;

impl FnLower<'_, '_> {
    /// `consume` each of `es` in order, snapshotting a value that a later one may change
    /// (`[xs[0], grow(xs)]` reads `xs[0]` before `grow` reallocates `xs`, #580).
    pub(super) fn consume_each(&mut self, es: &[hir::Expr]) -> Vec<Operand> {
        // The elements of an array literal going into the caller's container in a `read`
        // callback (caller_stores.rs `Held::Elems`).
        let to_caller = std::mem::take(&mut self.read_info.elems_to_caller);
        let later = self.later_each(es);
        // A move out of a part of a local a later operand reads (#2258, literal_moves.rs).
        let parts = match to_caller {
            true => vec![None; es.len()],
            false => self.part_moves_read_later(es),
        };
        let mut vals = Vec::with_capacity(es.len());
        let mut deferred = vec![];
        for (i, ((e, later), part)) in es.iter().zip(later).zip(parts).enumerate() {
            let v = match part {
                _ if to_caller => self.keep_for_caller(e),
                Some(_) if self.shareable_part(e) => self.consume_shared(e),
                Some(l) if !later.may_write(l) => {
                    deferred.push(i);
                    vals.push(unit());
                    continue;
                }
                _ => self.consume(e),
            };
            vals.push(self.hold_owned(v, e.ty, later));
        }
        for i in deferred {
            vals[i] = self.consume(&es[i]);
        }
        vals
    }

    /// For each of `es`: the local it moves a part out of, when a later one reads that local.
    fn part_moves_read_later(&mut self, es: &[hir::Expr]) -> Vec<Option<LocalId>> {
        let roots: Vec<Option<LocalId>> = es.iter().map(|e| self.zeroing_move_root(e)).collect();
        let wanted: FxHashSet<LocalId> = roots.iter().flatten().copied().collect();
        if wanted.is_empty() {
            return vec![None; es.len()];
        }
        // One pass from the end: the wanted locals the operands after each one read.
        let mut out = vec![None; es.len()];
        let mut read = FxHashSet::default();
        for (i, e) in es.iter().enumerate().rev() {
            out[i] = roots[i].filter(|l| read.contains(l));
            reads(e, &wanted, &mut read);
        }
        out
    }

    /// Can the part `e` moves be shared instead? A string: a share is a reference of its own.
    /// A value holding objects is not shared here, as sharing it shares its parts with the
    /// local's (an inline object's fields); it is lowered later instead.
    fn shareable_part(&mut self, e: &hir::Expr) -> bool {
        let ty = self.sub(e.ty);
        matches!(self.cx.kind(ty), hir::TyKind::Str)
    }

    /// `e` for a `shareable_part`: the moved part read in place, then shared.
    fn consume_shared(&mut self, e: &hir::Expr) -> Operand {
        let mut borrowed = e.clone();
        set_outer_mode(&mut borrowed, UseMode::Borrow);
        let ty = self.sub(e.ty);
        let v = self.expr(&borrowed);
        let held = self.share_value(v.clone(), ty);
        match &held {
            Operand::Copy(h) if h.proj.is_empty() && held != v => {
                self.own_temp(h.local, ty);
                held
            }
            _ => self.own_value(held, ty),
        }
    }

    /// The local `e` moves a part out of, when the move empties or drops that part in the
    /// local itself (`field_expr`): not a whole local, not a field the local just stops
    /// dropping (`moved_fields`), not a proxied field (read through the proxy).
    fn zeroing_move_root(&mut self, e: &hir::Expr) -> Option<LocalId> {
        use hir::ExprKind as K;
        match &e.kind {
            K::Field {
                base,
                mode: UseMode::Move,
                ..
            } => {
                let bty = self.sub(base.ty);
                if self.cx.proxiable(bty) {
                    return None;
                }
                if let K::Local(id, _) = &base.kind {
                    if !self.info[id.0 as usize].zero_parts {
                        return None;
                    }
                }
                self.droppable_root(base)
            }
            K::UnwrapSome(base, UseMode::Move)
            | K::UnwrapVariant {
                expr: base,
                mode: UseMode::Move,
                ..
            } => match base.kind {
                K::Local(..) => None,
                _ => self.droppable_root(base),
            },
            _ => None,
        }
    }

    /// The local `base` is a part of, when this function owns it.
    fn droppable_root(&self, base: &hir::Expr) -> Option<LocalId> {
        part_root(base).filter(|id| self.info[id.0 as usize].droppable)
    }
}

/// Add to `read` the locals of `wanted` that evaluating `e` may read (all of them for a shape
/// `any_operand` does not look into).
fn reads(e: &hir::Expr, wanted: &FxHashSet<LocalId>, read: &mut FxHashSet<LocalId>) {
    use hir::ExprKind as K;
    match &e.kind {
        K::Lit(_) | K::Global(_) | K::FnRef(..) => {}
        K::Local(l, _) => {
            if wanted.contains(l) {
                read.insert(*l);
            }
        }
        K::AdtLit { fields: xs, .. } | K::Tuple(xs) => {
            for x in xs {
                reads(x, wanted, read);
            }
        }
        _ => {
            let known = any_operand(e, &mut |x| {
                reads(x, wanted, read);
                false
            });
            if known.is_none() {
                read.extend(wanted.iter().copied());
            }
        }
    }
}

/// Make the outermost projection of `e` a `mode` read.
fn set_outer_mode(e: &mut hir::Expr, mode: UseMode) {
    use hir::ExprKind as K;
    match &mut e.kind {
        K::Field { mode: m, .. } | K::UnwrapSome(_, m) | K::UnwrapVariant { mode: m, .. } => {
            *m = mode
        }
        _ => {}
    }
}
