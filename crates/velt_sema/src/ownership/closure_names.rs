//! Which closure a local names (`exclusive`): a closure bound to a local (`const f = () => …`,
//! a nested `function` that captures, `crate::nested_closures`, or another member of its
//! recursive group, `Intrinsic::ClosureSibling`), followed through the captures of the closures
//! that call it by that name.

use rustc_hash::FxHashMap;

use crate::ctx::Ctx;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, LocalId, StmtKind as S};
use crate::visit::{self, VisitMut};

/// Which closure a local is bound to (`const f = () => …`, a nested `function`), and which
/// function creates each closure.
pub(super) struct Closures {
    bound: FxHashMap<(DefId, LocalId), DefId>,
    parent: FxHashMap<DefId, DefId>,
}

impl Closures {
    pub(super) fn of(cx: &Ctx) -> Closures {
        let mut out = Closures {
            bound: FxHashMap::default(),
            parent: FxHashMap::default(),
        };
        for &d in &cx.fn_defs {
            let Some(Def::Fn(f)) = &cx.defs[d.0 as usize] else {
                continue;
            };
            let mut block = f.body.block.clone();
            let mut v = Bound { d, out: &mut out };
            visit::block(&mut block, &mut v);
        }
        out
    }

    /// The closure local `l` of function `g` (body `f`) holds, if it is bound to one there or
    /// is a capture of a variable bound to one in an enclosing function.
    /// `caps`: `g`'s captures (inner, outer).
    pub(super) fn resolve(
        &self,
        cx: &Ctx,
        g: DefId,
        caps: &[(LocalId, LocalId)],
        l: LocalId,
    ) -> Option<DefId> {
        let (mut g, mut l) = (g, l);
        let mut caps = caps.to_vec();
        for _ in 0..16 {
            if let Some(c) = self.bound.get(&(g, l)) {
                return Some(*c);
            }
            let outer = caps.iter().find(|(inner, _)| *inner == l)?.1;
            g = *self.parent.get(&g)?;
            l = outer;
            caps = match &cx.defs[g.0 as usize] {
                Some(Def::Fn(p)) => p.captures.iter().map(|c| (c.inner, c.outer)).collect(),
                _ => vec![],
            };
        }
        None
    }
}

/// Collects [`Closures`] of one function.
struct Bound<'a> {
    d: DefId,
    out: &'a mut Closures,
}

impl VisitMut for Bound<'_> {
    fn stmt(&mut self, s: &mut crate::hir::Stmt) {
        if let S::Let {
            local,
            init: Some(init),
        } = &s.kind
        {
            let c = match &init.kind {
                E::Closure(c) => Some(*c),
                // Another member of a recursive closure group (`crate::nested_closures`).
                E::Call {
                    callee: Callee::Intrinsic(Intrinsic::ClosureSibling(c)),
                    ..
                } => Some(*c),
                _ => None,
            };
            if let Some(c) = c {
                self.out.bound.insert((self.d, *local), c);
            }
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        match &e.kind {
            E::Closure(c) => {
                self.out.parent.insert(*c, self.d);
            }
            // A nested function called before its declaration is assigned where it is declared.
            E::Assign { place, value } => {
                if let (E::Local(l, _), E::Closure(c)) = (&place.kind, &value.kind) {
                    self.out.bound.insert((self.d, *l), *c);
                }
            }
            _ => {}
        }
    }
}
