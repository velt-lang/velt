//! An outside object a `with` callback stores into the locked value is stored as a copy
//! (`super::stores`): the variable keeps the original. Using the variable after the `with`
//! then sees an object the value no longer holds, unlike JavaScript, so such a later use is an
//! error, like a variable used after `shared(x)` took it (super module docs).

use velt_common::Span;

use crate::ctx::Ctx;
use crate::hir::{Def, DefId, Expr, ExprKind as E, LocalId, Stmt, StmtKind as S};
use crate::visit::{self, VisitMut};

/// A use of local `l` of function `f` after `at` (the callback storing it): later in the
/// source, or anywhere in a loop around `at` (the next iteration).
pub(super) fn later_use(cx: &mut Ctx, f: DefId, l: LocalId, at: Span) -> Option<Span> {
    let Some(Def::Fn(mut body)) = cx.defs[f.0 as usize].take() else {
        return None;
    };
    let mut uses = Uses {
        local: l,
        captures: vec![],
        loops: vec![],
        uses: vec![],
    };
    // Closures capturing `l` use it where they are made.
    visit::block(&mut body.body.block, &mut uses);
    cx.defs[f.0 as usize] = Some(Def::Fn(body));
    let Uses {
        captures,
        loops,
        uses,
        ..
    } = uses;
    let mut all = uses;
    for (n, span) in captures {
        if let Some(Def::Fn(c)) = &cx.defs[n.0 as usize] {
            if c.captures.iter().any(|k| k.outer == l) {
                all.push(span);
            }
        }
    }
    let inside = |s: Span, o: Span| o.lo <= s.lo && s.hi <= o.hi;
    let around: Vec<Span> = loops.into_iter().filter(|lp| inside(at, *lp)).collect();
    all.into_iter()
        .filter(|u| !inside(*u, at))
        .filter(|u| u.lo >= at.hi || around.iter().any(|lp| inside(*u, *lp)))
        .min_by_key(|u| u.lo)
}

struct Uses {
    local: LocalId,
    /// Closures made in the body, with their spans.
    captures: Vec<(DefId, Span)>,
    /// Spans of the loops in the body.
    loops: Vec<Span>,
    uses: Vec<Span>,
}

impl VisitMut for Uses {
    fn stmt(&mut self, s: &mut Stmt) {
        if matches!(s.kind, S::While { .. } | S::ForOf { .. }) {
            self.loops.push(s.span);
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        match e.kind {
            E::Local(l, _) if l == self.local => self.uses.push(e.span),
            E::Closure(n) => self.captures.push((n, e.span)),
            _ => {}
        }
    }
}
