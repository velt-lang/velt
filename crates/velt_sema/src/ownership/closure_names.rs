//! Which closure a local names (`exclusive`): a closure bound to a local (`const f = () => …`,
//! a nested `function` that captures, `crate::nested_closures`, or another member of its
//! recursive group, `Intrinsic::ClosureSibling`), followed through the captures of the closures
//! that call it by that name.

use rustc_hash::FxHashMap;

use super::local_closures::walk;
use crate::ctx::Ctx;
use crate::hir::{
    Block, Callee, Capture, Def, DefId, Expr, ExprKind as E, Intrinsic, LocalId, StmtKind as S,
};

/// Which closure a local is bound to (`const f = () => …`, a nested `function`), and which
/// function creates each closure.
#[derive(Default)]
pub(super) struct Closures {
    bound: FxHashMap<(DefId, LocalId), DefId>,
    parent: FxHashMap<DefId, DefId>,
}

impl Closures {
    pub(super) fn of(cx: &Ctx) -> Closures {
        let mut out = Closures::default();
        for &d in &cx.fn_defs {
            // Std's own closures are not followed: a call of one by the name it is bound to
            // keeps every conflict (`exclusive::Checker::relaxed_callee`). A program's closures
            // are created in the program's functions.
            if cx.modules[cx.fn_info(d).module].is_std {
                continue;
            }
            if let Some(Def::Fn(f)) = &cx.defs[d.0 as usize] {
                out.add(d, &f.body.block);
            }
        }
        out
    }

    /// The closures function `d` (body `b`) creates and binds.
    fn add(&mut self, d: DefId, b: &Block) {
        walk::block(b, &mut Bound { d, out: self });
    }

    /// The closure local `l` of function `g` holds, if it is bound to one there or is a
    /// capture of a variable bound to one in an enclosing function. `caps`: `g`'s captures;
    /// `caps_of`: those of an enclosing function (`g`'s own definition may be taken out of the
    /// context while it is checked).
    pub(super) fn resolve<'c>(
        &self,
        g: DefId,
        caps: &'c [Capture],
        l: LocalId,
        caps_of: impl Fn(DefId) -> &'c [Capture],
    ) -> Option<DefId> {
        let (mut g, mut l, mut caps) = (g, l, caps);
        for _ in 0..16 {
            if let Some(c) = self.bound.get(&(g, l)) {
                return Some(*c);
            }
            let outer = caps.iter().find(|c| c.inner == l)?.outer;
            g = *self.parent.get(&g)?;
            l = outer;
            caps = caps_of(g);
        }
        None
    }
}

/// Collects [`Closures`] of one function.
struct Bound<'a> {
    d: DefId,
    out: &'a mut Closures,
}

impl walk::Visit for Bound<'_> {
    fn stmt(&mut self, s: &crate::hir::Stmt) {
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

    fn expr(&mut self, e: &Expr) {
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

#[cfg(test)]
mod tests {
    use velt_common::{FileId, Span};

    use super::*;
    use crate::hir::{PassMode, Stmt, TyId, UseMode};

    fn e(kind: E) -> Expr {
        Expr {
            kind,
            ty: TyId(0),
            span: Span::new(FileId(0), 0, 0),
        }
    }

    fn block(stmts: Vec<S>) -> Block {
        let span = Span::new(FileId(0), 0, 0);
        let stmts = stmts.into_iter().map(|kind| Stmt { kind, span }).collect();
        Block {
            stmts,
            value: None,
            span,
        }
    }

    fn bind(local: u32, init: E) -> S {
        S::Let {
            local: LocalId(local),
            init: Some(e(init)),
        }
    }

    fn cap(outer: u32, inner: u32) -> Capture {
        Capture {
            outer: LocalId(outer),
            inner: LocalId(inner),
            mode: PassMode::Borrow,
            share: false,
        }
    }

    /// `function make() { const g = (x) => …; const f = (a) => { g(a); }; }`: `make` (def 0)
    /// binds `g` (local 0) to closure 1 and creates closure 2 (`f`), which captures `g` as its
    /// local 3, and calls it through a closure 3 it creates, which captures that as local 5.
    #[test]
    fn a_capture_names_the_closure_its_variable_is_bound_to() {
        let mut c = Closures::default();
        c.add(
            DefId(0),
            &block(vec![
                bind(0, E::Closure(DefId(1))),
                bind(1, E::Closure(DefId(2))),
            ]),
        );
        c.add(DefId(2), &block(vec![S::Expr(e(E::Closure(DefId(3))))]));
        let in_f = [cap(0, 3)];
        let in_inner = [cap(3, 5)];
        let caps_of = |d: DefId| -> &[Capture] {
            match d.0 {
                2 => &in_f,
                3 => &in_inner,
                _ => &[],
            }
        };
        assert_eq!(
            c.resolve(DefId(0), &[], LocalId(0), caps_of),
            Some(DefId(1))
        );
        assert_eq!(
            c.resolve(DefId(2), &in_f, LocalId(3), caps_of),
            Some(DefId(1))
        );
        assert_eq!(
            c.resolve(DefId(3), &in_inner, LocalId(5), caps_of),
            Some(DefId(1))
        );
        // `f` itself, and a local that is neither bound to a closure nor a capture.
        assert_eq!(
            c.resolve(DefId(0), &[], LocalId(1), caps_of),
            Some(DefId(2))
        );
        assert_eq!(c.resolve(DefId(2), &in_f, LocalId(4), caps_of), None);
        // A capture of a variable that holds no closure.
        let other = [cap(7, 3)];
        assert_eq!(c.resolve(DefId(2), &other, LocalId(3), |_| &[]), None);
    }

    /// Two nested functions that call each other: the group's members are bound in each other
    /// through `ClosureSibling` (`crate::nested_closures`).
    #[test]
    fn a_sibling_names_the_other_member_of_its_group() {
        let sibling = |d: u32, arg: u32| E::Call {
            callee: Callee::Intrinsic(Intrinsic::ClosureSibling(DefId(d))),
            args: vec![e(E::Local(LocalId(arg), UseMode::Borrow))],
        };
        let mut c = Closures::default();
        c.add(DefId(0), &block(vec![bind(0, E::Closure(DefId(1)))]));
        c.add(DefId(1), &block(vec![bind(2, sibling(4, 1))]));
        assert_eq!(
            c.resolve(DefId(1), &[], LocalId(2), |_| &[]),
            Some(DefId(4))
        );
        assert_eq!(c.resolve(DefId(1), &[], LocalId(1), |_| &[]), None);
        // A nested function assigned where it is declared, after a call above it.
        let assign = E::Assign {
            place: Box::new(e(E::Local(LocalId(6), UseMode::Move))),
            value: Box::new(e(E::Closure(DefId(5)))),
        };
        c.add(
            DefId(0),
            &block(vec![
                S::Let {
                    local: LocalId(6),
                    init: None,
                },
                S::Expr(e(assign)),
            ]),
        );
        assert_eq!(
            c.resolve(DefId(0), &[], LocalId(6), |_| &[]),
            Some(DefId(5))
        );
    }
}
