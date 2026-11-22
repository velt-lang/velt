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

use rustc_hash::{FxHashMap, FxHashSet};

use crate::body::LocalKind;
use crate::ctx::Ctx;
use crate::hir::{Block, Callee, Def, Expr, ExprKind as E, LocalId, StmtKind as S, UseMode};

use super::walk;

/// One walk of a function's body for the calls of all its `const`-held closures: per closure,
/// the variables a reference into which is held while one of its calls runs, or which its
/// arguments mention. A closure's calls are fine when none of them is one it borrows (or a
/// borrowing pattern binding, which may point into one). Walking once per closure with the
/// variables it borrows cost the square of a function holding many closures.
pub(super) struct Sites<'a, 'm> {
    cx: &'a Ctx<'m>,
    /// The locals holding the closures.
    closures: &'a FxHashSet<LocalId>,
    /// The role of each local of the function.
    kinds: &'a [LocalKind],
    /// The variables references into which are held at this point of the walk (repeats
    /// allowed).
    held: Vec<LocalId>,
    /// Per closure local: the variables held or mentioned at its calls.
    pub at_calls: FxHashMap<LocalId, FxHashSet<LocalId>>,
    /// Expressions looked at (`crate::check_work`).
    pub steps: u64,
}

impl<'a, 'm> Sites<'a, 'm> {
    pub(super) fn new(
        cx: &'a Ctx<'m>,
        closures: &'a FxHashSet<LocalId>,
        kinds: &'a [LocalKind],
    ) -> Self {
        Sites {
            cx,
            closures,
            kinds,
            held: vec![],
            at_calls: FxHashMap::default(),
            steps: 0,
        }
    }

    /// The variables closure `k` captures.
    fn captures(&self, k: crate::hir::DefId, out: &mut Vec<LocalId>) {
        if let Some(Def::Fn(kf)) = &self.cx.defs[k.0 as usize] {
            out.extend(kf.captures.iter().map(|c| c.outer));
        }
    }

    /// The variables `e`, as an operand, may be a reference into: the root of a place that is
    /// not read as a copy, or what a closure captures.
    fn holds(&self, e: &Expr, out: &mut Vec<LocalId>) {
        match &e.kind {
            E::Upcast(x) | E::Downcast(x) | E::WrapSome(x) | E::ToDyn { expr: x, .. } => {
                self.holds(x, out)
            }
            E::If { then, els, .. } => {
                self.holds(then, out);
                self.holds(els, out);
            }
            E::Closure(k) => self.captures(*k, out),
            _ => {
                if let Some((root, mode)) = place(e) {
                    if mode != UseMode::Copy {
                        out.push(root);
                    }
                }
            }
        }
    }

    /// The variables `e` mentions anywhere (other than as a copied read).
    fn mentions(&self, e: &Expr, out: &mut Vec<LocalId>) {
        struct Mentions<'s, 'a, 'm>(&'s Sites<'a, 'm>, &'s mut Vec<LocalId>);
        impl walk::Visit for Mentions<'_, '_, '_> {
            fn expr(&mut self, x: &Expr) {
                match &x.kind {
                    E::Local(l, m) if *m != UseMode::Copy => self.1.push(*l),
                    E::Closure(k) => self.0.captures(*k, self.1),
                    _ => {}
                }
            }
        }
        walk::expr(e, &mut Mentions(self, out));
    }

    /// Hold the references `e` may be for the rest of the current scope; returns where to
    /// truncate `held` to let them go.
    fn hold(&mut self, e: &Expr) -> usize {
        let mark = self.held.len();
        let mut roots = std::mem::take(&mut self.held);
        self.holds(e, &mut roots);
        self.held = roots;
        mark
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

    pub(super) fn block(&mut self, b: &Block) {
        let mark = self.held.len();
        for s in &b.stmts {
            match &s.kind {
                S::Let { init, .. } => {
                    if let Some(e) = init {
                        self.expr(e);
                    }
                }
                S::LetPat { init, .. } => {
                    self.expr(init);
                    // Held for the rest of the block.
                    self.hold(init);
                }
                S::Expr(e) | S::Return(Some(e)) => self.expr(e),
                S::Return(None) | S::Break(_) | S::Continue(_) => {}
                S::If { cond, then, els } => {
                    self.expr(cond);
                    self.block(then);
                    if let Some(b) = els {
                        self.block(b);
                    }
                }
                S::While {
                    cond, body, step, ..
                } => {
                    self.expr(cond);
                    self.block(body);
                    if let Some(e) = step {
                        self.expr(e);
                    }
                }
                S::ForOf { iter, body, .. } => {
                    self.expr(iter);
                    let inner = self.hold(iter);
                    self.block(body);
                    self.held.truncate(inner);
                }
                S::Try {
                    body,
                    catch,
                    finally,
                } => {
                    self.block(body);
                    if let Some((_, b)) = catch {
                        self.block(b);
                    }
                    if let Some(b) = finally {
                        self.block(b);
                    }
                }
                S::Block(b) => self.block(b),
            }
        }
        if let Some(e) = &b.value {
            self.expr(e);
        }
        self.held.truncate(mark);
    }

    fn expr(&mut self, e: &Expr) {
        self.steps += 1;
        match &e.kind {
            E::Call { callee, args } => {
                if let Callee::Indirect(c) = callee {
                    match c.kind {
                        E::Local(l, _) if self.closures.contains(&l) => self.call(l, args),
                        _ => self.expr(c),
                    }
                }
                self.args(args);
            }
            E::New { args, .. } => self.args(args),
            E::CompoundAssign { place, value, .. } => {
                self.expr(place);
                let mark = self.held.len();
                if !self.stable_place(place) {
                    self.hold(place);
                }
                self.expr(value);
                self.held.truncate(mark);
            }
            E::Index { base, index, .. } => {
                self.expr(base);
                let mark = self.hold(base);
                self.expr(index);
                self.held.truncate(mark);
            }
            E::Match { scrutinee, arms } => {
                self.expr(scrutinee);
                let mark = self.hold(scrutinee);
                for a in arms {
                    if let Some(g) = &a.guard {
                        self.expr(g);
                    }
                    self.expr(&a.body);
                }
                self.held.truncate(mark);
            }
            E::Block(b) => self.block(b),
            _ => walk::children(e, &mut |x| self.expr(x)),
        }
    }

    /// A call of the closure held in `local`: what is held now, and what its arguments mention
    /// (the call borrows the captures while its arguments are live).
    fn call(&mut self, local: LocalId, args: &[Expr]) {
        let mut seen = self.held.clone();
        for a in args {
            self.mentions(a, &mut seen);
        }
        self.at_calls.entry(local).or_default().extend(seen);
    }

    /// Each argument, with the references its siblings hold during the call.
    fn args(&mut self, args: &[Expr]) {
        let holding: Vec<Vec<LocalId>> = args
            .iter()
            .map(|a| {
                let mut out = vec![];
                self.holds(a, &mut out);
                out
            })
            .collect();
        for (i, a) in args.iter().enumerate() {
            let mark = self.held.len();
            for (j, h) in holding.iter().enumerate() {
                if j != i {
                    self.held.extend(h);
                }
            }
            self.expr(a);
            self.held.truncate(mark);
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
