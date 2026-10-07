//! Closures whose environment can live in the creating function's frame whatever their capture
//! modes (#34): the closure value never outlives the frame, because it is only ever called
//! there. Two shapes qualify (a lowering layout choice; HIR is unchanged):
//! - an immediately called closure (`(() => x * k)()`): `Call { callee: Indirect(Closure) }`;
//! - a closure literal initializing a `let` / `const` local whose every use is the callee of a
//!   call through it (`const f = (x) => x * k; f(1); f(2)`), and that no closure captures.
//!
//! A call `spawn` starts (`spawn(f())`, either branch of `spawn(c ? f() : g())`) is not such a
//! call: the task gets a copy of the function value (async_fn/spawn.rs, callee.rs
//! `call_indirect`), which a frame env cannot give.
//!
//! Neither may be an async or generator closure: calling one creates a promise or generator
//! that keeps (or sends to another thread) the closure's environment beyond the call. Async
//! functions and generators themselves are lowered as state machines whose frames move between
//! polls; they are never scanned (`lower_fn` handles them before reaching the scan).
//!
//! Such an env is a frame temporary laid out like a heap env (closure.rs): Copy captures are
//! values, owned captures and cells are moved or retained into it, and its drop entry is a
//! frame drop function (`Work::EnvDropFrame`) that releases them without freeing the env,
//! called when the closure value is dropped. Its clone and transfer entries are null: nothing
//! copies or transfers a closure that is only called.

use std::collections::{HashMap, HashSet};

use velt_sema::hir::{
    self, Callee, DefId, Expr, ExprKind as E, Intrinsic, LocalId, StmtKind as S,
};

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
    /// Neither async nor a generator: calling it runs it to completion.
    fn plain(&self, d: DefId) -> bool {
        matches!(self.hir.def(d), hir::Def::Fn(c) if !c.is_async && !c.is_generator)
    }

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
                        if self.plain(d) {
                            self.lets.insert(*local, d);
                        }
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
                    E::Closure(d) if self.plain(d) => {
                        self.immediate.insert(d);
                    }
                    _ => {}
                }
                self.expr(c);
                args.iter().for_each(|a| self.expr(a));
            }
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::Spawn | Intrinsic::SpawnHandled),
                args,
            } => args.iter().for_each(|a| self.spawned(a)),
            E::Block(b) => self.block(b),
            _ => children(e, &mut |x| self.expr(x)),
        }
    }

    /// The promise `spawn` starts (async_fn/spawn.rs): a call through a function value there is
    /// a use of the callee, not a call, in either branch of a conditional and through a block
    /// holding only a value.
    fn spawned(&mut self, e: &Expr) {
        match &e.kind {
            E::If { cond, then, els } => {
                self.expr(cond);
                self.spawned(then);
                self.spawned(els);
            }
            E::Block(b) if b.stmts.is_empty() && b.value.is_some() => {
                b.value.iter().for_each(|v| self.spawned(v));
            }
            E::Call {
                callee: Callee::Indirect(c),
                args,
            } => {
                self.expr(c);
                args.iter().for_each(|a| self.expr(a));
            }
            _ => self.expr(e),
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
