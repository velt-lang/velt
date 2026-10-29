//! A `read` callback's variables naming the caller's objects (caller_stores.rs module docs):
//! bound to a part of what the callback captured from code outside read mode, or to what a
//! method or function takes out of or reads from such a part (`const g = first(mine)`). Code
//! changing what they name changes the caller's containers.

use rustc_hash::FxHashSet;
use velt_sema::hir::{self, ExprKind as K, Intrinsic, LocalId};

use super::frozen::place_root;
use super::FnLower;

/// What `caller_alias` and `call_alias` found in a `read` callback's body, once asked.
#[derive(Default)]
pub(super) struct Aliases {
    /// Bound to a place rooted in a captured variable of the caller's, or in another of these.
    places: FxHashSet<LocalId>,
    /// Bound to a call's result that `caller_object` finds the caller's (`first(mine)`).
    calls: Option<FxHashSet<LocalId>>,
    /// Those calls' variables, never given another value, with the call.
    candidates: Vec<(LocalId, hir::Expr)>,
    /// The other variables bound to a place and never given another value, with its root: one
    /// rooted in a call's variable is one too (`const k = g`).
    rest: Vec<(LocalId, LocalId)>,
}

impl FnLower<'_, '_> {
    /// Is `l` a variable of this `read` callback naming one of the caller's objects: bound to
    /// a part of what the callback captured (`captured_root`, or of another such variable),
    /// directly or as a `for…of` element, and never given another value? Code changing what it
    /// names changes the caller's containers.
    pub(super) fn caller_alias(&mut self, l: LocalId) -> bool {
        if self.caller_aliases.is_none() {
            let found = match self.cx.cur_def.map(|d| self.cx.hir.def(d)) {
                Some(hir::Def::Fn(f)) => {
                    let caps: Vec<LocalId> = f.captures.iter().map(|c| c.inner).collect();
                    let captures = caps.into_iter().filter(|&c| self.caller_capture(c));
                    let mut b = Bindings {
                        captures: captures.collect(),
                        ..Bindings::default()
                    };
                    b.block(&f.body.block);
                    b.aliases()
                }
                _ => Aliases::default(),
            };
            self.caller_aliases = Some(found);
        }
        self.caller_aliases
            .as_ref()
            .is_some_and(|a| a.places.contains(&l))
    }

    /// Is `l` a variable of this `read` callback bound to what a method or function takes out
    /// of or reads from the caller's container (`caller_object`: `const g = first(mine)`), and
    /// never given another value? It may also be an object the call made (`LOCAL`), so what
    /// changes it asks which at run time (array/ops.rs `caller_or_own_op`).
    pub(super) fn call_alias(&mut self, l: LocalId) -> bool {
        self.caller_alias(l);
        let Some(a) = self.caller_aliases.as_mut() else {
            return false;
        };
        if a.calls.is_none() {
            let candidates = std::mem::take(&mut a.candidates);
            let rest = std::mem::take(&mut a.rest);
            a.calls = Some(FxHashSet::default());
            // Each call's first argument may be another of these variables.
            loop {
                let mut grew = false;
                for (v, init) in &candidates {
                    // Only a lock-set object tells at run time whether the call made it.
                    let t = self.sub(self.info[v.0 as usize].ty);
                    if self.call_alias_known(*v)
                        || self.lock_ref(t).is_none()
                        || !self.caller_object(init)
                    {
                        continue;
                    }
                    grew |= self.add_call_alias(*v);
                }
                for &(v, root) in &rest {
                    let t = self.sub(self.info[v.0 as usize].ty);
                    if self.call_alias_known(root) && self.lock_ref(t).is_some() {
                        grew |= self.add_call_alias(v);
                    }
                }
                if !grew {
                    break;
                }
            }
        }
        self.call_alias_known(l)
    }

    fn add_call_alias(&mut self, l: LocalId) -> bool {
        let calls = self.caller_aliases.as_mut().and_then(|a| a.calls.as_mut());
        calls.is_some_and(|c| c.insert(l))
    }

    fn call_alias_known(&self, l: LocalId) -> bool {
        self.caller_aliases
            .as_ref()
            .and_then(|a| a.calls.as_ref())
            .is_some_and(|c| c.contains(&l))
    }
}

/// What `caller_alias` looks at in a body.
#[derive(Default)]
struct Bindings {
    captures: FxHashSet<LocalId>,
    /// Each variable bound to a place, with the place's root.
    bound: Vec<(LocalId, LocalId)>,
    /// Each variable bound to a method's or function's result given a place first.
    called: Vec<(LocalId, hir::Expr)>,
    /// Variables given a value more than once (assigned, or bound twice), or bound to no place.
    changed: FxHashSet<LocalId>,
    /// Variables assigned.
    assigned: FxHashSet<LocalId>,
}

impl Bindings {
    /// `l` bound to `init` (`call`: a variable's own value, not an element of `init`).
    fn bind(&mut self, l: LocalId, init: &hir::Expr, call: bool) {
        if self.bound.iter().any(|(b, _)| *b == l) || self.called.iter().any(|(b, _)| *b == l) {
            self.changed.insert(l);
        }
        let init = unshared(init);
        if let Some(root) = place_root(init) {
            return self.bound.push((l, root));
        }
        self.changed.insert(l);
        // `mine.at(0)!`: the payload of such a call (`caller_object` checks the arms).
        let called = match &init.kind {
            K::Match { scrutinee, .. } => scrutinee,
            _ => init,
        };
        if let (
            true,
            K::Call {
                callee: hir::Callee::Def(..) | hir::Callee::Intrinsic(_),
                args,
            },
        ) = (call, &called.kind)
        {
            if args.first().is_some_and(|a| place_root(a).is_some()) {
                self.called.push((l, init.clone()));
            }
        }
    }

    fn aliases(self) -> Aliases {
        let mut out = FxHashSet::default();
        loop {
            let before = out.len();
            for &(l, root) in &self.bound {
                if !self.changed.contains(&l)
                    && (self.captures.contains(&root) || out.contains(&root))
                {
                    out.insert(l);
                }
            }
            if out.len() == before {
                break;
            }
        }
        // Bound once: `bind` marked each of them changed for `places`.
        let once = |l: &LocalId| {
            self.bound.iter().all(|(b, _)| b != l)
                && self.called.iter().filter(|(b, _)| b == l).count() == 1
                && !self.assigned.contains(l)
        };
        let candidates = self.called.iter().filter(|(l, _)| once(l)).cloned();
        let rest = self
            .bound
            .iter()
            .filter(|(l, _)| !self.changed.contains(l) && !out.contains(l));
        Aliases {
            candidates: candidates.collect(),
            rest: rest.copied().collect(),
            places: out,
            calls: None,
        }
    }
}

impl Bindings {
    fn block(&mut self, b: &hir::Block) {
        b.stmts.iter().for_each(|s| self.stmt(s));
        if let Some(e) = &b.value {
            self.expr(e);
        }
    }

    fn stmt(&mut self, s: &hir::Stmt) {
        use hir::StmtKind as S;
        match &s.kind {
            S::Let { local, init } => {
                if let Some(init) = init {
                    self.bind(*local, init, true);
                    self.expr(init);
                }
            }
            S::LetPat { pat, init } => {
                if let hir::PatKind::Binding(l, _) = pat.kind {
                    self.bind(l, init, true);
                }
                self.expr(init);
            }
            S::Expr(init) | S::Return(Some(init)) => self.expr(init),
            S::Return(None) | S::Break(_) | S::Continue(_) => {}
            S::If { cond, then, els } => {
                self.expr(cond);
                self.block(then);
                els.iter().for_each(|b| self.block(b));
            }
            S::While {
                cond, body, step, ..
            } => {
                self.expr(cond);
                self.block(body);
                step.iter().for_each(|e| self.expr(e));
            }
            S::ForOf {
                binding,
                iter,
                body,
                ..
            } => {
                if let hir::PatKind::Binding(l, _) = binding.kind {
                    self.bind(l, iter, false);
                }
                self.expr(iter);
                self.block(body);
            }
            S::Try {
                body,
                catch,
                finally,
            } => {
                self.block(body);
                catch.iter().for_each(|(_, b)| self.block(b));
                finally.iter().for_each(|b| self.block(b));
            }
            S::Block(b) => self.block(b),
        }
    }

    fn expr(&mut self, e: &hir::Expr) {
        if let K::Assign { place, .. } | K::CompoundAssign { place, .. } = &e.kind {
            if let K::Local(l, _) = place.kind {
                self.changed.insert(l);
                self.assigned.insert(l);
            }
        }
        match &e.kind {
            K::Block(b) => self.block(b),
            _ => super::frame_envs::children(e, &mut |x| self.expr(x)),
        }
    }
}

/// Does `p` bind `l`?
pub(super) fn binds(p: &hir::Pat, l: LocalId) -> bool {
    use hir::PatKind as P;
    match &p.kind {
        P::Binding(b, _) => *b == l,
        P::Some(p) => binds(p, l),
        P::Variant { args: ps, .. } | P::Tuple(ps) | P::Or(ps) => ps.iter().any(|p| binds(p, l)),
        P::Adt { fields } => fields.iter().any(|(_, p)| binds(p, l)),
        P::Array { elems, rest } => *rest == Some(l) || elems.iter().any(|p| binds(p, l)),
        _ => false,
    }
}

/// `e` without a `Share` around it.
fn unshared(e: &hir::Expr) -> &hir::Expr {
    match &e.kind {
        K::Call {
            callee: hir::Callee::Intrinsic(Intrinsic::Share),
            args,
        } if args.len() == 1 => &args[0],
        _ => e,
    }
}
