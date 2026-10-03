//! Which closures a function value may be (super module docs): a closure literal, either
//! branch of a conditional, a local bound only to closure literals, a variable a closure
//! captured (resolved where the closure is made), a parameter of a function (the closures
//! every call passes for it, when every call is a direct one), and a call's result (what the
//! function may return). Anything else — a field, an
//! array element, a parameter of a closure or a method — is not resolved.

use std::collections::{HashMap, HashSet};

use crate::ctx::Ctx;
use crate::defs::{BodyState, FnKind};
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, LocalId};
use crate::visit;

/// Resolves function values to closures, caching what it learns about the program.
#[derive(Default)]
pub(super) struct Resolver {
    /// The body making each closure (filled on first use).
    parents: Option<HashMap<DefId, DefId>>,
    /// The closures each `(function, parameter)` may be given (`None`: not resolved).
    params: HashMap<(DefId, usize), Option<Vec<DefId>>>,
    /// Parameters being resolved (a recursive function passing its parameter on).
    pending: HashSet<(DefId, usize)>,
    /// Functions whose results are being resolved.
    returning: HashSet<DefId>,
}

impl Resolver {
    /// The closures `e`, evaluated in body `d`, may be.
    pub(super) fn expr(&mut self, cx: &mut Ctx, d: DefId, e: &Expr) -> Option<Vec<DefId>> {
        match &e.kind {
            E::Closure(n) => Some(vec![*n]),
            E::If { then, els, .. } => {
                let mut a = self.expr(cx, d, then)?;
                a.extend(self.expr(cx, d, els)?);
                Some(a)
            }
            E::Local(l, _) => self.local(cx, d, *l),
            E::Call {
                callee: Callee::Def(g, _),
                ..
            } => self.returned(cx, *g),
            _ => None,
        }
    }

    /// The closures function `g` may return (what each `return` may be).
    fn returned(&mut self, cx: &mut Ctx, g: DefId) -> Option<Vec<DefId>> {
        struct Returns(Vec<Expr>, bool);
        impl visit::VisitMut for Returns {
            fn stmt(&mut self, s: &mut crate::hir::Stmt) {
                if let crate::hir::StmtKind::Return(e) = &s.kind {
                    match e {
                        Some(e) => self.0.push(e.clone()),
                        None => self.1 = true,
                    }
                }
            }
        }
        if !self.returning.insert(g) {
            return Some(vec![]);
        }
        let Some(Def::Fn(mut f)) = cx.defs[g.0 as usize].take() else {
            return None;
        };
        let mut r = Returns(vec![], false);
        visit::block(&mut f.body.block, &mut r);
        if let Some(v) = &f.body.block.value {
            r.0.push((**v).clone());
        }
        cx.defs[g.0 as usize] = Some(Def::Fn(f));
        let mut out = (!r.1).then_some(vec![]);
        for e in r.0 {
            match (self.expr(cx, g, &e), &mut out) {
                (Some(cs), Some(acc)) => acc.extend(cs),
                _ => out = None,
            }
        }
        self.returning.remove(&g);
        out
    }

    /// The closures local `l` of body `d` may be.
    pub(super) fn local(&mut self, cx: &mut Ctx, d: DefId, l: LocalId) -> Option<Vec<DefId>> {
        let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
            return None;
        };
        let pos = f.params.iter().position(|p| p.local == l);
        let ncap = f.captures.len();
        let captured = pos.filter(|&i| i < ncap).map(|i| f.captures[i].outer);
        let bound = match pos {
            None => super::callbacks::closure_locals(&mut f.body.block).remove(&l),
            Some(_) => None,
        };
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
        match (pos, captured) {
            (_, Some(outer)) => {
                let parent = self.parent(cx, d)?;
                self.local(cx, parent, outer)
            }
            (Some(i), None) => self.param(cx, d, i),
            (None, _) => bound,
        }
    }

    /// The closures every direct call of function `d` passes for its parameter `i`.
    fn param(&mut self, cx: &mut Ctx, d: DefId, i: usize) -> Option<Vec<DefId>> {
        if !matches!(cx.fn_info(d).kind, FnKind::Free | FnKind::Static) {
            return None;
        }
        if let Some(r) = self.params.get(&(d, i)) {
            return r.clone();
        }
        if !self.pending.insert((d, i)) {
            // Passed on to itself: what the other calls pass.
            return Some(vec![]);
        }
        let mut out = Some(vec![]);
        for (caller, arg) in call_args(cx, d, i) {
            match (self.expr(cx, caller, &arg), &mut out) {
                (Some(cs), Some(acc)) => acc.extend(cs),
                _ => out = None,
            }
        }
        self.pending.remove(&(d, i));
        self.params.insert((d, i), out.clone());
        out
    }

    fn parent(&mut self, cx: &mut Ctx, n: DefId) -> Option<DefId> {
        if self.parents.is_none() {
            self.parents = Some(closure_parents(cx));
        }
        self.parents.as_ref()?.get(&n).copied()
    }
}

/// The body making each closure of the program.
fn closure_parents(cx: &mut Ctx) -> HashMap<DefId, DefId> {
    let mut out = HashMap::new();
    for d in done_fns(cx) {
        let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            if let E::Closure(n) = e.kind {
                out.insert(n, d);
            }
        });
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
    }
    out
}

/// Argument `i` of every direct call of `d`, with the calling body.
fn call_args(cx: &mut Ctx, d: DefId, i: usize) -> Vec<(DefId, Expr)> {
    let mut out = vec![];
    for caller in done_fns(cx) {
        let Some(Def::Fn(mut f)) = cx.defs[caller.0 as usize].take() else {
            continue;
        };
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            if let E::Call {
                callee: Callee::Def(g, _),
                args,
            } = &e.kind
            {
                if *g == d {
                    if let Some(a) = args.get(i) {
                        out.push((caller, a.clone()));
                    }
                }
            }
        });
        cx.defs[caller.0 as usize] = Some(Def::Fn(f));
    }
    out
}

fn done_fns(cx: &Ctx) -> Vec<DefId> {
    cx.fn_defs
        .iter()
        .copied()
        .filter(|d| cx.fn_info(*d).state == BodyState::Done)
        .collect()
}
