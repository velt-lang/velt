//! Where the calls of a `const`-held closure run (see the parent module): none may run while a
//! reference into a variable the closure borrows is held, since the call may change or replace
//! what the reference points to.
//!
//! A reference is held across the evaluation of an expression `x` by an argument next to `x`
//! that is a place rooted at such a variable (or a closure capturing one), by the place of a
//! compound assignment whose value is `x` (unless it is a field of `this` or of a `const`
//! object, whose address the call cannot change), by the array indexed with `x`, by a `match`
//! on such a place (its arms), by a `for...of` over one (its body), and by a destructuring or
//! by-reference declaration of one (the rest of its block). The call's own arguments must not
//! mention the borrowed variables at all: the call borrows them while its arguments are live.
//! Borrowing pattern bindings (`for...of` elements, destructured names) may point into those
//! variables, so they count as them. The check is syntactic and conservative: it runs before
//! ownership inference, when pass modes are not known yet.

use std::collections::HashSet;

use crate::body::LocalKind;
use crate::ctx::Ctx;
use crate::hir::{Block, Callee, Def, Expr, ExprKind as E, LocalId, StmtKind as S, UseMode};

use super::walk;

/// The call-site walker for the closure held in `local`.
pub(super) struct Sites<'a, 'm> {
    cx: &'a Ctx<'m>,
    local: LocalId,
    /// The enclosing variables the closure captures by reference.
    borrowed: &'a HashSet<LocalId>,
    /// Borrowing pattern bindings of the function (they may point into `borrowed`).
    aliases: &'a HashSet<LocalId>,
    /// The role of each local of the function.
    kinds: &'a [LocalKind],
    /// No call found so far runs while a reference is held.
    pub ok: bool,
}

impl<'a, 'm> Sites<'a, 'm> {
    pub(super) fn new(
        cx: &'a Ctx<'m>,
        local: LocalId,
        borrowed: &'a HashSet<LocalId>,
        aliases: &'a HashSet<LocalId>,
        kinds: &'a [LocalKind],
    ) -> Self {
        Sites {
            cx,
            local,
            borrowed,
            aliases,
            kinds,
            ok: true,
        }
    }

    fn watched(&self, l: LocalId) -> bool {
        self.borrowed.contains(&l) || self.aliases.contains(&l)
    }

    /// Does closure `k` capture a watched variable?
    fn captures_watched(&self, k: crate::hir::DefId) -> bool {
        match &self.cx.defs[k.0 as usize] {
            Some(Def::Fn(kf)) => kf.captures.iter().any(|c| self.watched(c.outer)),
            _ => false,
        }
    }

    /// Is `e`, as an operand, a reference into a watched variable: a place rooted at one that
    /// is not read as a copy, or a closure capturing one?
    fn holds(&self, e: &Expr) -> bool {
        match &e.kind {
            E::Upcast(x) | E::Downcast(x) | E::WrapSome(x) | E::ToDyn { expr: x, .. } => {
                self.holds(x)
            }
            E::If { then, els, .. } => self.holds(then) || self.holds(els),
            E::Closure(k) => self.captures_watched(*k),
            _ => place(e).is_some_and(|(root, mode)| mode != UseMode::Copy && self.watched(root)),
        }
    }

    /// Does `e` mention a watched variable anywhere (other than as a copied read)?
    fn mentions(&self, e: &Expr) -> bool {
        struct Mentions<'s, 'a, 'm>(&'s Sites<'a, 'm>, bool);
        impl walk::Visit for Mentions<'_, '_, '_> {
            fn expr(&mut self, x: &Expr) {
                self.1 |= match &x.kind {
                    E::Local(l, m) => *m != UseMode::Copy && self.0.watched(*l),
                    E::Closure(k) => self.0.captures_watched(*k),
                    _ => false,
                };
            }
        }
        let mut v = Mentions(self, false);
        walk::expr(e, &mut v);
        v.1
    }

    /// A field of `this` or of a `const` object local: the call can neither replace nor move
    /// the object, so the field's address stays valid.
    fn stable_place(&self, e: &Expr) -> bool {
        let E::Field { base, .. } = &e.kind else {
            return false;
        };
        let E::Local(l, _) = base.kind else {
            return false;
        };
        let kind = self.kinds.get(l.0 as usize);
        self.cx.class_of(base.ty).is_some()
            && matches!(kind, Some(LocalKind::This | LocalKind::Const))
    }

    pub(super) fn block(&mut self, b: &Block, held: bool) {
        let mut held = held;
        for s in &b.stmts {
            match &s.kind {
                S::Let { init, .. } => {
                    if let Some(e) = init {
                        self.expr(e, held);
                    }
                }
                S::LetPat { init, .. } => {
                    self.expr(init, held);
                    held |= self.holds(init);
                }
                S::Expr(e) | S::Return(Some(e)) => self.expr(e, held),
                S::Return(None) | S::Break(_) | S::Continue(_) => {}
                S::If { cond, then, els } => {
                    self.expr(cond, held);
                    self.block(then, held);
                    if let Some(b) = els {
                        self.block(b, held);
                    }
                }
                S::While {
                    cond, body, step, ..
                } => {
                    self.expr(cond, held);
                    self.block(body, held);
                    if let Some(e) = step {
                        self.expr(e, held);
                    }
                }
                S::ForOf { iter, body, .. } => {
                    self.expr(iter, held);
                    let inner = held || self.holds(iter);
                    self.block(body, inner);
                }
                S::Try {
                    body,
                    catch,
                    finally,
                } => {
                    self.block(body, held);
                    if let Some((_, b)) = catch {
                        self.block(b, held);
                    }
                    if let Some(b) = finally {
                        self.block(b, held);
                    }
                }
                S::Block(b) => self.block(b, held),
            }
        }
        if let Some(e) = &b.value {
            self.expr(e, held);
        }
    }

    fn expr(&mut self, e: &Expr, held: bool) {
        match &e.kind {
            E::Call { callee, args } => {
                if let Callee::Indirect(c) = callee {
                    if matches!(c.kind, E::Local(l, _) if l == self.local) {
                        self.ok &= !held && !args.iter().any(|a| self.mentions(a));
                    } else {
                        self.expr(c, held);
                    }
                }
                self.args(args, held);
            }
            E::New { args, .. } => self.args(args, held),
            E::CompoundAssign { place, value, .. } => {
                self.expr(place, held);
                let inner = held || (self.holds(place) && !self.stable_place(place));
                self.expr(value, inner);
            }
            E::Index { base, index, .. } => {
                self.expr(base, held);
                let inner = held || self.holds(base);
                self.expr(index, inner);
            }
            E::Match { scrutinee, arms } => {
                self.expr(scrutinee, held);
                let inner = held || self.holds(scrutinee);
                for a in arms {
                    if let Some(g) = &a.guard {
                        self.expr(g, inner);
                    }
                    self.expr(&a.body, inner);
                }
            }
            E::Block(b) => self.block(b, held),
            _ => walk::children(e, &mut |x| self.expr(x, held)),
        }
    }

    /// Each argument, with the references its siblings hold during the call.
    fn args(&mut self, args: &[Expr], held: bool) {
        let holding: Vec<bool> = args.iter().map(|a| self.holds(a)).collect();
        for (i, a) in args.iter().enumerate() {
            let others = holding.iter().enumerate().any(|(j, h)| *h && j != i);
            self.expr(a, held || others);
        }
    }
}

/// `(root, mode)` of a place expression (`Local` / `Field` / `Index` / unwraps), with the mode
/// of its outermost node.
fn place(e: &Expr) -> Option<(LocalId, UseMode)> {
    let mode = match &e.kind {
        E::Local(_, m)
        | E::Field { mode: m, .. }
        | E::Index { mode: m, .. }
        | E::UnwrapSome(_, m)
        | E::UnwrapVariant { mode: m, .. } => *m,
        E::Downcast(x) => return place(x),
        _ => return None,
    };
    let mut cur = e;
    loop {
        cur = match &cur.kind {
            E::Local(l, _) => return Some((*l, mode)),
            E::Field { base, .. }
            | E::Index { base, .. }
            | E::UnwrapSome(base, _)
            | E::UnwrapVariant { expr: base, .. }
            | E::Downcast(base) => base,
            _ => return None,
        };
    }
}
