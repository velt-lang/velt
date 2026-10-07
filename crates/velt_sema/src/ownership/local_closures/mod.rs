//! Closures held in a `const` that are only ever called (docs/reference/functions.md
//! "Captures"): `const f = () => this.items.length; f();`. Checking treats every closure that
//! is not passed directly as an argument as escaping (it captures by value, and a by-value
//! capture of `this` or of an object makes its type reference-counted). This pass, run before
//! ownership inference, finds the ones that provably cannot outlive the function creating them
//! and makes them non-escaping: they capture by reference and their environment lives in the
//! frame, as a closure argument's does.
//!
//! A closure bound by `const f = <arrow>` is demoted when:
//! - the enclosing function is neither async nor a generator (a frame env must not live across
//!   an `await` or a `yield`), and the closure is neither;
//! - every use of `f` is the callee of a direct call `f(...)`: `f` is never copied, stored,
//!   returned, passed on or captured by another closure (itself included);
//! - it captures no `using` variable and no value holding a promise (those are moved, never
//!   shared, so a capture by reference could see them moved away), and no closure created in
//!   it captures a variable the enclosing function assigns (that closure may escape with a
//!   copy, which only a cell shared through an escaping closure keeps up to date);
//! - no call `f(...)` runs while a reference into one of its captured objects is held
//!   ([`sites`]): its own arguments do not mention them, and it is not evaluated next to an
//!   argument that borrows one, inside a `for...of` over one, a `match` on one, after a
//!   destructuring or by-reference `const` of one, or as an index into one.
//!
//! The move analysis then treats each call of `f` as a use of what it borrows
//! (`crate::moves`), and the exclusive-access check as an argument passing its captures
//! (`super::exclusive`), so a capture moved before a call becomes a share, and a call that
//! would invalidate a held reference is an error rather than undefined behaviour.

mod sites;
mod walk;

use std::collections::{HashMap, HashSet};

use crate::body::LocalKind;
use crate::ctx::Ctx;
use crate::defs::BodyState;
use crate::hir::{
    Block, Callee, Def, DefId, Expr, ExprKind as E, FnDef, LocalId, PassMode, Stmt, StmtKind as S,
};

/// Demote the `const`-bound closures of every function that are only called (module docs).
pub(crate) fn demote_local_closures(cx: &mut Ctx) {
    let fns: Vec<DefId> = cx
        .fn_defs
        .iter()
        .copied()
        .filter(|d| cx.fn_info(*d).state == BodyState::Done)
        .collect();
    for d in fns {
        let Some(Def::Fn(f)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        if !f.is_async && !f.is_generator {
            for (local, c) in candidates(cx, d, &f) {
                if qualifies(cx, d, &f, local, c) {
                    demote(cx, &f, c);
                }
            }
        }
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
    }
}

/// The closures of body `b` that are held in a local and do not escape (demoted by this pass),
/// by local. A call through such a local runs the closure on its borrowed captures.
pub(crate) fn held_closures(cx: &Ctx, b: &Block) -> HashMap<LocalId, DefId> {
    let mut out = HashMap::new();
    for (local, c) in closure_lets(b) {
        if cx.try_fn(c).is_some_and(|i| !i.escaping) {
            out.insert(local, c);
        }
    }
    out
}

/// Every `let` / `const` of `b` initialized with a closure literal.
fn closure_lets(b: &Block) -> Vec<(LocalId, DefId)> {
    struct Lets(Vec<(LocalId, DefId)>);
    impl walk::Visit for Lets {
        fn stmt(&mut self, s: &Stmt) {
            if let S::Let {
                local,
                init:
                    Some(Expr {
                        kind: E::Closure(c),
                        ..
                    }),
            } = &s.kind
            {
                self.0.push((*local, *c));
            }
        }
    }
    let mut v = Lets(vec![]);
    walk::block(b, &mut v);
    v.0
}

/// `const f = <arrow>` declarations of `d`'s body whose closure was checked as escaping.
fn candidates(cx: &Ctx, d: DefId, f: &FnDef) -> Vec<(LocalId, DefId)> {
    let kinds = &cx.fn_info(d).local_kinds;
    closure_lets(&f.body.block)
        .into_iter()
        .filter(|&(local, c)| {
            let info = cx.fn_info(c);
            let plain = !info.is_async && !info.is_generator && !info.is_async_gen;
            kinds.get(local.0 as usize) == Some(&LocalKind::Const) && plain && info.escaping
        })
        .collect()
}

/// May closure `c`, held in `local` of `d`'s body, capture by reference (module docs)?
fn qualifies(cx: &mut Ctx, d: DefId, f: &FnDef, local: LocalId, c: DefId) -> bool {
    let Some(Def::Fn(cf)) = &cx.defs[c.0 as usize] else {
        return false;
    };
    if cf.is_async || cf.is_generator {
        return false;
    }
    let caps: Vec<(LocalId, crate::hir::TyId)> = cf
        .captures
        .iter()
        .map(|cap| (cap.outer, cf.body.locals[cap.inner.0 as usize].ty))
        .collect();
    // A closure created inside it that captures one of its variables keeps it by value when it
    // escapes; if the enclosing function assigns that variable, the two must share a cell,
    // which only a closure capturing by value can pass on.
    let reassigned: HashSet<LocalId> = cf
        .captures
        .iter()
        .filter(|cap| assigned(cx, &f.body.block, cap.outer))
        .map(|cap| cap.inner)
        .collect();
    if !reassigned.is_empty() && recaptures(cx, &cf.body.block, &reassigned) {
        return false;
    }
    let kinds = cx.fn_info(d).local_kinds.clone();
    let mut borrowed = HashSet::new();
    for &(outer, ty) in &caps {
        if outer == local || kinds.get(outer.0 as usize) == Some(&LocalKind::Using) {
            return false;
        }
        if !cx.is_copy(ty) {
            if !cx.is_shared_value(ty) {
                return false;
            }
            borrowed.insert(outer);
        }
    }
    if !only_called(cx, &f.body.block, local) {
        return false;
    }
    let aliases: HashSet<LocalId> = (0..kinds.len())
        .filter(|&i| matches!(kinds[i], LocalKind::Bind | LocalKind::Elem))
        .map(|i| LocalId(i as u32))
        .collect();
    let mut s = sites::Sites::new(cx, local, &borrowed, &aliases, &kinds);
    s.block(&f.body.block, false);
    s.ok
}

/// Is every use of `local` in `b` the callee of a direct call, and no closure captures it?
fn only_called(cx: &Ctx, b: &Block, local: LocalId) -> bool {
    let (mut uses, mut calls, mut captured) = (0usize, 0usize, false);
    each_expr(b, &mut |e: &Expr| match &e.kind {
        E::Local(l, _) if *l == local => uses += 1,
        E::Call {
            callee: Callee::Indirect(callee),
            ..
        } if matches!(callee.kind, E::Local(l, _) if l == local) => calls += 1,
        E::Closure(k) => {
            if let Some(Def::Fn(kf)) = &cx.defs[k.0 as usize] {
                captured |= kf.captures.iter().any(|cap| cap.outer == local);
            }
        }
        _ => {}
    });
    !captured && uses == calls
}

/// Does a closure created in `b` capture one of `locals`?
fn recaptures(cx: &Ctx, b: &Block, locals: &HashSet<LocalId>) -> bool {
    let mut hit = false;
    each_expr(b, &mut |e: &Expr| {
        if let E::Closure(k) = e.kind {
            if let Some(Def::Fn(kf)) = &cx.defs[k.0 as usize] {
                hit |= kf.captures.iter().any(|cap| locals.contains(&cap.outer));
            }
        }
    });
    hit
}

/// Make closure `c` (created in `f`) non-escaping: it captures by reference (mutably where its
/// body assigns the variable), except Copy values that are never assigned, which stay copies.
/// Modifications of a captured object's contents raise `Borrow` later, in mutation inference.
fn demote(cx: &mut Ctx, f: &FnDef, c: DefId) {
    let Some(Def::Fn(cf)) = &cx.defs[c.0 as usize] else {
        return;
    };
    let caps: Vec<(LocalId, LocalId, crate::hir::TyId)> = cf
        .captures
        .iter()
        .map(|cap| {
            (
                cap.outer,
                cap.inner,
                cf.body.locals[cap.inner.0 as usize].ty,
            )
        })
        .collect();
    let mut modes = vec![];
    for (outer, inner, ty) in caps {
        let own_write = match &cx.defs[c.0 as usize] {
            Some(Def::Fn(cf)) => assigned(cx, &cf.body.block, inner),
            _ => true,
        };
        modes.push(if own_write {
            PassMode::BorrowMut
        } else if cx.is_copy(ty) && !assigned(cx, &f.body.block, outer) {
            PassMode::Copy
        } else {
            PassMode::Borrow
        });
    }
    if let Some(Def::Fn(cf)) = &mut cx.defs[c.0 as usize] {
        for (k, mode) in modes.into_iter().enumerate() {
            cf.captures[k].mode = mode;
            cf.captures[k].share = false;
            cf.params[k].mode = mode;
        }
    }
    cx.fn_info_mut(c).escaping = false;
}

/// Is local `l` assigned anywhere in `b`, directly or by a closure capturing it?
fn assigned(cx: &Ctx, b: &Block, l: LocalId) -> bool {
    let mut hit = false;
    each_expr(b, &mut |e: &Expr| match &e.kind {
        E::Assign { place, .. } | E::CompoundAssign { place, .. } => {
            hit |= matches!(place.kind, E::Local(x, _) if x == l);
        }
        E::Closure(k) => {
            if let Some(Def::Fn(kf)) = &cx.defs[k.0 as usize] {
                for cap in kf.captures.iter().filter(|cap| cap.outer == l) {
                    hit |= assigned(cx, &kf.body.block, cap.inner);
                }
            }
        }
        _ => {}
    });
    hit
}

/// Calls `f` on every expression of `b`.
fn each_expr(b: &Block, f: &mut dyn FnMut(&Expr)) {
    struct Exprs<'a>(&'a mut dyn FnMut(&Expr));
    impl walk::Visit for Exprs<'_> {
        fn expr(&mut self, e: &Expr) {
            (self.0)(e);
        }
    }
    walk::block(b, &mut Exprs(f));
}
