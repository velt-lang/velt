//! Closures whose environment can live in the creating function's frame whatever their capture
//! modes (#34): the closure value never outlives the frame, because it is only ever called
//! there. Two shapes qualify (docs/internals/contracts/hir_encodings.md, "Function values"):
//! - an immediately called closure (`(() => x * k)()`): `Call { callee: Indirect(Closure) }`;
//! - a closure literal initializing a `let` / `const` local whose every use is the callee of a
//!   call through it (`const f = (x) => x * k; f(1); f(2)`), and that no closure captures.
//!
//! Async functions and generators are lowered as state machines whose frames move between
//! polls; they are never scanned (`lower_fn` handles them before reaching the scan). The env's
//! value captures are owned by the frame env, dropped with the closure value (closure.rs).

use std::collections::{HashMap, HashSet};

use velt_sema::hir::{self, Callee, DefId, Expr, ExprKind as E, LocalId, StmtKind as S};

/// The closures of `f`'s body that get a frame environment.
pub(super) fn scan(hir: &hir::Program, f: &hir::FnDef) -> HashSet<DefId> {
    let mut s = Scan {
        hir,
        lets: HashMap::new(),
        uses: HashMap::new(),
        calls: HashMap::new(),
        captured: HashSet::new(),
        immediate: HashSet::new(),
    };
    s.block(&f.body.block);
    let mut out = s.immediate;
    for (local, def) in s.lets {
        let uses = s.uses.get(&local).copied().unwrap_or(0);
        let calls = s.calls.get(&local).copied().unwrap_or(0);
        if uses == calls && !s.captured.contains(&local) {
            out.insert(def);
        }
    }
    out
}

struct Scan<'h> {
    hir: &'h hir::Program,
    /// Locals initialized with a closure literal.
    lets: HashMap<LocalId, DefId>,
    /// Per local: every use, and the uses that are the callee of a call.
    uses: HashMap<LocalId, usize>,
    calls: HashMap<LocalId, usize>,
    /// Locals some closure captures.
    captured: HashSet<LocalId>,
    /// Immediately called closures.
    immediate: HashSet<DefId>,
}

impl Scan<'_> {
    fn block(&mut self, b: &hir::Block) {
        for s in &b.stmts {
            self.stmt(s);
        }
        if let Some(e) = &b.value {
            self.expr(e);
        }
    }

    fn stmt(&mut self, s: &hir::Stmt) {
        match &s.kind {
            S::Let { local, init } => {
                if let Some(e) = init {
                    if let E::Closure(d) = e.kind {
                        self.lets.insert(*local, d);
                    }
                    self.expr(e);
                }
            }
            S::LetPat { init, .. } | S::Expr(init) | S::Return(Some(init)) => self.expr(init),
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
                self.block(body);
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

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            E::Local(l, _) => *self.uses.entry(*l).or_default() += 1,
            E::Closure(d) => {
                if let hir::Def::Fn(c) = self.hir.def(*d) {
                    self.captured.extend(c.captures.iter().map(|cap| cap.outer));
                }
            }
            E::Call {
                callee: Callee::Indirect(c),
                args,
            } => {
                match c.kind {
                    E::Local(l, _) => *self.calls.entry(l).or_default() += 1,
                    E::Closure(d) => {
                        self.immediate.insert(d);
                    }
                    _ => {}
                }
                self.expr(c);
                args.iter().for_each(|a| self.expr(a));
            }
            E::Block(b) => self.block(b),
            _ => children(e, &mut |x| self.expr(x)),
        }
    }
}

/// The direct sub-expressions of `e` (blocks, closures and calls through function values are
/// handled by the caller).
fn children(e: &Expr, f: &mut dyn FnMut(&Expr)) {
    match &e.kind {
        E::Lit(_) | E::Global(_) | E::FnRef(..) | E::Closure(_) | E::Local(..) | E::Block(_) => {}
        E::Unary { expr: x, .. }
        | E::Cast(x)
        | E::Await(x)
        | E::WrapSome(x)
        | E::UnwrapSome(x, _)
        | E::UnwrapVariant { expr: x, .. }
        | E::Upcast(x)
        | E::Downcast(x)
        | E::ToDyn { expr: x, .. }
        | E::Throw(x)
        | E::Field { base: x, .. } => f(x),
        E::Binary { lhs, rhs, .. } | E::Logical { lhs, rhs, .. } => {
            f(lhs);
            f(rhs);
        }
        E::Assign { place, value } | E::CompoundAssign { place, value, .. } => {
            f(place);
            f(value);
        }
        E::Index { base, index, .. } => {
            f(base);
            f(index);
        }
        E::Call { callee, args } => {
            if let Callee::Indirect(c) = callee {
                f(c);
            }
            args.iter().for_each(&mut *f);
        }
        E::If { cond, then, els } => {
            f(cond);
            f(then);
            f(els);
        }
        E::AdtLit { fields: xs, .. }
        | E::Variant { args: xs, .. }
        | E::ArrayLit(xs)
        | E::Tuple(xs)
        | E::New { args: xs, .. } => xs.iter().for_each(&mut *f),
        E::Match { scrutinee, arms } => {
            f(scrutinee);
            for a in arms {
                if let Some(g) = &a.guard {
                    f(g);
                }
                f(&a.body);
            }
        }
    }
}
