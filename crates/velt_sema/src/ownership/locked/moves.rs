//! Variables a `with` callback written at the call takes along into the lock (super module
//! docs): one it only reads, that the function making the callback does not use afterwards, is
//! captured by move instead of by reference. The callback's environment is transferred when the
//! callback is made, before the lock is taken — a move where this task held the only reference
//! (count 1, checked at run time), a copy of only what is still shared — and dropped while the
//! lock is still held (velt_vir async_fn/sync.rs `mutex_with`). So every reference the variable
//! held is released under the lock, and a part of it the callback stores into the value
//! (`m.with((d) => run(d, commands))`, where `run` keeps an argument) needs no copy: the
//! region of such a capture is `MOVED` (`super::regions`), which crosses into the outside
//! state but not into the value. Without this, the whole variable was copied at each call
//! that might store a part of it, whatever the call then stored.
//!
//! A variable moves only when that is invisible to the program: a local declared by `let` /
//! `const` or a parameter the function owns; not in a cell; not captured by another closure;
//! not used after the callback (`super::later`); and, when the `with` is in a loop, declared in
//! that loop (each iteration has its own). A resource without `clone()` keeps its reference
//! capture (moving it would make a shared one panic in the transfer instead of being an
//! error). A callback that makes a closure or a promise keeps all of its captures as they are:
//! those could keep a moved part beyond the lock.

use rustc_hash::FxHashSet;

use velt_common::Span;

use super::later::later_use;
use super::values::Resolver;
use crate::ctx::Ctx;
use crate::hir::{Def, DefId, Expr, ExprKind as E, FnDef, LocalId, PassMode, Stmt, StmtKind as S};
use crate::visit::{self, VisitMut};

/// Make the captures of callback `c` (a literal written as `with`'s argument) that may move
/// into the lock moving ones (module docs); the capture locals (of `c`) that are moved, also
/// those `c` already took by move.
pub(super) fn into_lock(cx: &mut Ctx, res: &mut Resolver, c: DefId) -> FxHashSet<LocalId> {
    let mut moved = FxHashSet::default();
    let Some(f) = res.parent(cx, c) else {
        return moved;
    };
    if cx.scopes[cx.fn_info(c).module].is_std {
        return moved;
    }
    let at = cx.fn_info(c).span;
    let Some(Def::Fn(mut cb)) = cx.defs[c.0 as usize].take() else {
        return moved;
    };
    let mut makes = Makes { cx, found: false };
    visit::block(&mut cb.body.block, &mut makes);
    let keep_all = makes.found;
    let mut candidates = vec![];
    for (k, cap) in cb.captures.iter().enumerate() {
        if cap.mode == PassMode::Owned && !cap.share {
            moved.insert(cap.inner);
            continue;
        }
        let ty = cb.body.locals[cap.inner.0 as usize].ty;
        let worth = cap.mode == PassMode::Borrow
            && !cb.body.locals[cap.inner.0 as usize].boxed
            && cx.is_shared_value(ty)
            && !cx.is_string_value(ty)
            && !cx.owns_uncopyable(ty);
        if worth && !keep_all {
            candidates.push(k);
        }
    }
    cx.defs[c.0 as usize] = Some(Def::Fn(cb));
    let mut take = vec![];
    for k in candidates {
        let outer = match &cx.defs[c.0 as usize] {
            Some(Def::Fn(cb)) => cb.captures[k].outer,
            _ => continue,
        };
        if movable(cx, f, c, outer, at) && later_use(cx, f, outer, at).is_none() {
            take.push(k);
        }
    }
    if let Some(Def::Fn(cb)) = &mut cx.defs[c.0 as usize] {
        for k in take {
            let cap = &mut cb.captures[k];
            cap.mode = PassMode::Owned;
            cap.share = false;
            moved.insert(cap.inner);
        }
    }
    moved
}

/// Can local `l` of `f` move into callback `c` (made at `at`)? A `let` / `const` local or an
/// owned parameter, not in a cell, captured by no other closure, and declared inside every
/// loop around `at`.
fn movable(cx: &mut Ctx, f: DefId, c: DefId, l: LocalId, at: Span) -> bool {
    let Some(Def::Fn(mut body)) = cx.defs[f.0 as usize].take() else {
        return false;
    };
    let ok = movable_in(cx, &mut body, c, l, at);
    cx.defs[f.0 as usize] = Some(Def::Fn(body));
    ok
}

fn movable_in(cx: &Ctx, f: &mut FnDef, c: DefId, l: LocalId, at: Span) -> bool {
    if f.body.locals[l.0 as usize].boxed {
        return false;
    }
    let own_param = f.params[f.captures.len()..]
        .iter()
        .any(|p| p.local == l && p.mode == PassMode::Owned);
    let mut scan = Scan {
        cx,
        local: l,
        callback: c,
        declared: None,
        captured_elsewhere: false,
        loops: vec![],
    };
    visit::block(&mut f.body.block, &mut scan);
    let inside = |s: Span, o: Span| o.lo <= s.lo && s.hi <= o.hi;
    // A loop around the callback must declare the variable in its body: each iteration has its
    // own (one declared in a `for` header is the same variable for every iteration).
    let per_iteration = scan
        .loops
        .iter()
        .filter(|(lp, _)| inside(at, *lp))
        .all(|(_, body)| scan.declared.is_some_and(|d| inside(d, *body)));
    (scan.declared.is_some() || own_param) && !scan.captured_elsewhere && per_iteration
}

/// Where local `local` is declared, whether a closure other than the callback captures it, and
/// the loops of the body.
struct Scan<'a, 'm> {
    cx: &'a Ctx<'m>,
    local: LocalId,
    callback: DefId,
    /// The `let` / `const` statement declaring it.
    declared: Option<Span>,
    captured_elsewhere: bool,
    /// Each loop's span and its body's.
    loops: Vec<(Span, Span)>,
}

impl VisitMut for Scan<'_, '_> {
    fn stmt(&mut self, s: &mut Stmt) {
        match &s.kind {
            S::Let { local, .. } if *local == self.local => self.declared = Some(s.span),
            S::While { body, .. } | S::ForOf { body, .. } => self.loops.push((s.span, body.span)),
            _ => {}
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        if let E::Closure(n) = e.kind {
            if n != self.callback {
                if let Some(Def::Fn(g)) = &self.cx.defs[n.0 as usize] {
                    if g.captures.iter().any(|k| k.outer == self.local) {
                        self.captured_elsewhere = true;
                    }
                }
            }
        }
    }
}

/// Does a body make a closure or a promise?
struct Makes<'a, 'm> {
    cx: &'a mut Ctx<'m>,
    found: bool,
}

impl VisitMut for Makes<'_, '_> {
    fn expr(&mut self, e: &mut Expr) {
        if matches!(e.kind, E::Closure(_)) || self.cx.holds_promise(e.ty) {
            self.found = true;
        }
    }
}
