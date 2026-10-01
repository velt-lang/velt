//! Expressions in the moves dataflow: place paths, branches, matches, closure captures.

use velt_common::Span;

use super::state::{join, Flow, Path, UNWRAP, VARIANT};
use super::Moves;
use crate::hir::{Arm, Callee, DefId, Expr, ExprKind, LocalId, PassMode, UseMode};

/// `(local, path)` of a place made of `Local` / `Field` / `UnwrapSome` / `UnwrapVariant`
/// projections.
fn place_path(e: &Expr) -> Option<(LocalId, Path)> {
    match &e.kind {
        ExprKind::Local(l, _) => Some((*l, vec![])),
        ExprKind::Field { base, index, .. } => {
            let (l, mut p) = place_path(base)?;
            p.push(*index);
            Some((l, p))
        }
        ExprKind::UnwrapSome(base, _) => {
            let (l, mut p) = place_path(base)?;
            p.push(UNWRAP);
            Some((l, p))
        }
        ExprKind::UnwrapVariant { expr: base, .. } => {
            let (l, mut p) = place_path(base)?;
            p.push(VARIANT);
            Some((l, p))
        }
        _ => None,
    }
}

/// The path a move of `p` consumes: moving (a part of) a union member moves the union value
/// (lowering drops the rest of the member right away, like a `match` arm's bindings).
fn moved_path(mut p: Path) -> Path {
    if let Some(i) = p.iter().position(|x| *x == VARIANT) {
        p.truncate(i);
    }
    p
}

/// The place an operand moves as a whole (through value-preserving conversions), and its span.
fn moved_place(e: &Expr) -> Option<(LocalId, Path, Span)> {
    match &e.kind {
        ExprKind::WrapSome(x) | ExprKind::Upcast(x) | ExprKind::ToDyn { expr: x, .. } => {
            moved_place(x)
        }
        _ if outer_mode(e) == UseMode::Move => {
            place_path(e).map(|(l, p)| (l, moved_path(p), e.span))
        }
        _ => None,
    }
}

fn outer_mode(e: &Expr) -> UseMode {
    match e.kind {
        ExprKind::Local(_, m)
        | ExprKind::Field { mode: m, .. }
        | ExprKind::UnwrapSome(_, m)
        | ExprKind::UnwrapVariant { mode: m, .. } => m,
        _ => UseMode::Borrow,
    }
}

impl Moves<'_> {
    /// A use of place `e` as a whole; false if `e` is not rooted at a local.
    fn place(&mut self, e: &Expr, mode: UseMode, st: &mut Flow) -> bool {
        match place_path(e) {
            Some((l, p)) => {
                let p = if mode == UseMode::Move {
                    moved_path(p)
                } else {
                    p
                };
                self.use_path(l, &p, mode, e.span, st, false);
                true
            }
            None => false,
        }
    }

    pub(super) fn expr(&mut self, e: &Expr, st: &mut Flow) {
        // Only a closure that is the whole initializer has a known holder.
        let holder = self
            .holder
            .take()
            .filter(|_| matches!(e.kind, ExprKind::Closure(_)));
        match &e.kind {
            ExprKind::Lit(_) | ExprKind::Global(_) | ExprKind::FnRef(..) => {}
            ExprKind::Closure(d) => self.closure(*d, e.span, holder, st),
            ExprKind::Local(..)
            | ExprKind::Field { .. }
            | ExprKind::UnwrapSome(..)
            | ExprKind::UnwrapVariant { .. } => self.projection(e, st),
            ExprKind::Index { base, index, .. } => {
                if !self.place(base, UseMode::Borrow, st) {
                    self.expr(base, st);
                }
                self.expr(index, st);
            }
            ExprKind::Unary { expr, .. }
            | ExprKind::Cast(expr)
            | ExprKind::WrapSome(expr)
            | ExprKind::Await(expr)
            | ExprKind::Upcast(expr)
            | ExprKind::ToDyn { expr, .. }
            | ExprKind::Throw(expr) => self.expr(expr, st),
            ExprKind::Binary { lhs, rhs, .. } => {
                self.expr(lhs, st);
                self.expr(rhs, st);
            }
            ExprKind::Logical { lhs, rhs, .. } => {
                self.expr(lhs, st);
                let skip = st.clone();
                self.expr(rhs, st);
                *st = join(st.take(), skip);
            }
            ExprKind::Assign { place, value } => {
                self.expr(value, st);
                self.assign_place(place, st);
            }
            ExprKind::CompoundAssign { place, value, .. } => {
                self.expr(value, st);
                if let Some((l, _)) = place_path(place) {
                    self.assigned(l, place.span, st);
                }
                if !self.place(place, UseMode::Borrow, st) {
                    self.expr(place, st);
                }
            }
            ExprKind::Call { callee, args } => {
                if let Callee::Indirect(c) = callee {
                    self.expr(c, st);
                }
                self.operands(args, st);
            }
            ExprKind::If { cond, then, els } => {
                self.expr(cond, st);
                let mut other = st.clone();
                self.expr(then, st);
                self.expr(els, &mut other);
                *st = join(st.take(), other);
            }
            ExprKind::Block(b) => self.block(b, st),
            ExprKind::AdtLit { fields: xs, .. }
            | ExprKind::Variant { args: xs, .. }
            | ExprKind::ArrayLit(xs)
            | ExprKind::Tuple(xs)
            | ExprKind::New { args: xs, .. } => self.operands(xs, st),
            ExprKind::Match { scrutinee, arms } => self.match_arms(scrutinee, arms, st),
        }
        if e.ty == self.never {
            *st = None;
        }
    }

    /// Operands of one call / construction, evaluated left to right. A place moved as a whole
    /// operand is moved when the call happens, after every operand is evaluated (two-phase),
    /// so `m.set(k, m.get(k) + 1)` and `P { a: s, n: s.length }` may still read it.
    fn operands(&mut self, xs: &[Expr], st: &mut Flow) {
        let mut deferred = vec![];
        for x in xs {
            match moved_place(x) {
                Some((l, path, span)) => {
                    self.use_path(l, &path, UseMode::Borrow, span, st, false);
                    deferred.push((l, path, span));
                }
                None => self.expr(x, st),
            }
        }
        for (l, path, span) in deferred {
            self.use_path(l, &path, UseMode::Move, span, st, false);
        }
    }

    /// A read of a place (a projection of a local, or of a temporary).
    fn projection(&mut self, e: &Expr, st: &mut Flow) {
        if self.place(e, outer_mode(e), st) {
            return;
        }
        if let ExprKind::Field { base, .. }
        | ExprKind::UnwrapSome(base, _)
        | ExprKind::UnwrapVariant { expr: base, .. } = &e.kind
        {
            self.expr(base, st);
        }
    }

    fn match_arms(&mut self, scrutinee: &Expr, arms: &[Arm], st: &mut Flow) {
        self.expr(scrutinee, st);
        let start = st.take();
        let mut out = None;
        for arm in arms {
            let mut s = start.clone();
            Self::init_pat(&arm.pat, &mut s);
            if let Some(g) = &arm.guard {
                self.expr(g, &mut s);
            }
            self.expr(&arm.body, &mut s);
            out = join(out, s);
        }
        *st = out;
    }

    /// Writing a place re-initializes it (a field of a wholly moved value is an error).
    fn assign_place(&mut self, place: &Expr, st: &mut Flow) {
        if let Some((l, _)) = place_path(place) {
            self.assigned(l, place.span, st);
        }
        match place_path(place) {
            Some((l, p)) if p.is_empty() => Self::init_local(l, st),
            Some((l, p)) => {
                self.use_path(l, &p[..p.len() - 1], UseMode::Borrow, place.span, st, false);
                if let Some(s) = st {
                    s.reinit(l.0 as usize, &p);
                }
            }
            None => self.expr(place, st),
        }
    }

    /// Creating a closure uses its captures: by-value captures move the variable (softly for
    /// shared values: a variable used again is shared with the closure instead).
    fn closure(&mut self, d: DefId, span: Span, holder: Option<LocalId>, st: &mut Flow) {
        let Some(caps) = self.captures.get(&d) else {
            return;
        };
        let escaping = self.escaping.contains(&d);
        let writes =
            |c: &crate::hir::Capture| self.writers.get(&d).is_some_and(|w| w.contains(&c.outer));
        for c in caps.clone() {
            let i = c.outer.0 as usize;
            // A second escaping closure capturing a variable that one of them assigns.
            if escaping && matches!(c.mode, PassMode::Owned | PassMode::Copy) && self.boxable[i] {
                let prev = st.as_ref().and_then(|s| s.captured[i]);
                if prev.is_some_and(|(_, _, w)| w || writes(&c)) {
                    self.boxed.insert(c.outer);
                }
            }
            let soft_share = self.soft.contains(&span) && self.shared[c.outer.0 as usize];
            let (mode, by_closure) = match c.mode {
                PassMode::Owned => (UseMode::Move, !soft_share),
                PassMode::Copy => (UseMode::Copy, false),
                PassMode::Borrow | PassMode::BorrowMut => (UseMode::Borrow, false),
            };
            if c.mode == PassMode::BorrowMut {
                self.assigned(c.outer, span, st);
            }
            self.use_path(c.outer, &[], mode, span, st, by_closure);
            if escaping && matches!(c.mode, PassMode::Owned | PassMode::Copy) {
                let w = writes(&c);
                if let Some(s) = st {
                    let before = s.captured[i].is_some_and(|(_, _, x)| x);
                    s.captured[i] = Some((span, holder.map(|h| h.0 as usize), w || before));
                }
            }
        }
    }
}
