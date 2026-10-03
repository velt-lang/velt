//! Promises made by a `with` callback from the locked value (super module docs): a call or
//! `new` whose result holds a promise and whose arguments (the receiver included) may reach
//! the locked value (`super::regions`), in the callback or a closure it makes, and a call of a
//! function that makes such a promise from an argument reaching it and leaves it running
//! (`super::summary`). The promise runs after the lock is released, so it would use the value
//! without the lock. When the promise only reads what it is given from the value (a function
//! that does not modify those parameters: `save(v.name)`, `read(v)`), it gets a copy instead,
//! like a spawned call (an error when the copy would need to duplicate a resource without
//! `clone()`); otherwise it is an error. Calling a function value kept in the value is
//! allowed (`m.with((f) => f())`), and so is calling an async closure that captured the
//! value: an async closure copies what it captured per call. `spawn` transfers what it is
//! given, so a spawned call is allowed too, and `Promise.all` and the other combinators only
//! wait for the promises they are given, each checked where it is made.

use std::collections::HashSet;

use velt_common::Span;

use super::regions::{Regions, IN};
use super::stores::wrap;
use super::summary::outer_mode;
use super::summary::Summaries;
use crate::body::places::is_place;
use crate::ctx::Ctx;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, FnDef, Intrinsic, TyId, UseMode};
use crate::visit::{self, VisitMut};

/// A promise made from the locked value.
pub(super) enum Made {
    /// A promise-making expression and its type.
    Here(Span, TyId),
    /// A promise that would need a copy of an argument owning a resource without `clone()`.
    Resource(Span, TyId),
    /// A call of a function that makes one and leaves it running.
    ByCall(Span, String),
}

/// The promises body `f` (of `def`) makes from the locked value; with `copy`, the ones that
/// only read it are given copies instead.
pub(super) fn made_from_value(
    cx: &mut Ctx,
    r: &Regions,
    s: &Summaries,
    def: DefId,
    f: &mut FnDef,
    copy: bool,
) -> Vec<Made> {
    let mut p = Promises {
        cx,
        r,
        s,
        def,
        copy,
        spawned: HashSet::new(),
        found: vec![],
    };
    visit::block(&mut f.body.block, &mut p);
    p.found
}

struct Promises<'a, 'r, 's, 'm> {
    cx: &'a mut Ctx<'m>,
    r: &'r Regions,
    s: &'s Summaries,
    def: DefId,
    copy: bool,
    /// Spans of the calls `spawn` takes.
    spawned: HashSet<Span>,
    found: Vec<Made>,
}

impl Promises<'_, '_, '_, '_> {
    fn reaches_value(&mut self, e: &Expr) -> bool {
        self.r.mentions(self.cx, self.def, e) & IN != 0
    }

    /// A call (or `new`) `e` of `callee` with `args`; may give some arguments copies.
    fn call(&mut self, e: &Expr, callee: &Callee, args: &mut [Expr]) {
        if self.spawned.contains(&e.span) {
            return;
        }
        // A closure resolved among the bodies makes its promises in its own body, checked
        // there (`super::values`).
        if let Callee::Indirect(c) = callee {
            if self.r.resolved.contains(&c.span) {
                return;
            }
        }
        if combinator(self.cx, callee) {
            // `Promise.all` and the like only wait for the promises they are given, each
            // checked where it is made.
            return;
        }
        if self.cx.holds_promise(e.ty) {
            let used: Vec<usize> = (0..args.len())
                .filter(|&i| self.reaches_value(&args[i]))
                .collect();
            if !used.is_empty() && !self.give_copies(callee, args, &used) {
                self.found.push(Made::Here(e.span, e.ty));
            }
            return;
        }
        let Callee::Def(g, _) = callee else { return };
        let promises = self.s.get(*g).map_or(0, |s| s.promises);
        let uses = (0..args.len().min(64))
            .filter(|i| promises & (1 << i) != 0)
            .any(|i| self.reaches_value(&args[i]));
        if uses {
            let name = self.cx.fn_info(*g).name.clone();
            self.found.push(Made::ByCall(e.span, name));
        }
    }

    /// Pass copies of the arguments at `used` to a known function that only reads those
    /// parameters (it does not modify them); false when it may modify one.
    fn give_copies(&mut self, callee: &Callee, args: &mut [Expr], used: &[usize]) -> bool {
        let Callee::Def(g, _) = callee else {
            return false;
        };
        let modified = match &self.cx.defs[g.0 as usize] {
            Some(Def::Fn(f)) => used.iter().any(|&i| {
                f.params
                    .get(i)
                    .is_none_or(|p| f.body.locals[p.local.0 as usize].mutable)
            }),
            _ => true,
        };
        if modified {
            return false;
        }
        // A copy of a resource without `clone()` cannot be made.
        if let Some(&i) = used.iter().find(|&&i| self.cx.owns_uncopyable(args[i].ty)) {
            self.found.push(Made::Resource(args[i].span, args[i].ty));
            return true;
        }
        if self.copy {
            for &i in used {
                copy_arg(self.cx, &mut args[i]);
            }
        }
        true
    }
}

/// `Promise.all`, `Promise.race`, `Promise.any` or `Promise.allSettled`.
fn combinator(cx: &Ctx, callee: &Callee) -> bool {
    match callee {
        Callee::Intrinsic(
            Intrinsic::PromiseAll | Intrinsic::PromiseRace | Intrinsic::PromiseAny,
        ) => true,
        Callee::Def(g, _) => {
            let info = cx.fn_info(*g);
            cx.scopes[info.module].is_std
                && matches!(
                    info.name.rsplit("::").next(),
                    Some("promiseAllSettled" | "promiseAny")
                )
        }
        _ => false,
    }
}

/// The argument `e` becomes a copy for the promise: a transferred share of a borrowed place,
/// or the transferred value.
fn copy_arg(cx: &mut Ctx, e: &mut Expr) {
    if !cx.is_shared_value(e.ty) || cx.is_string_value(e.ty) {
        return;
    }
    if is_place(e) && outer_mode(e) != Some(UseMode::Move) {
        wrap(e, Intrinsic::Share);
    }
    wrap(e, Intrinsic::Transfer);
}

impl VisitMut for Promises<'_, '_, '_, '_> {
    fn expr(&mut self, e: &mut Expr) {
        match &e.kind {
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::Spawn),
                args,
            } => {
                self.spawned.extend(args.iter().map(|a| a.span));
            }
            // Widens a promise made in its argument (found there).
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::PromiseWiden),
                ..
            } => {}
            E::Call { .. } | E::New { .. } => {
                let probe = Expr {
                    kind: E::Lit(crate::hir::Lit::Unit),
                    ty: e.ty,
                    span: e.span,
                };
                let (callee, mut args) = match &mut e.kind {
                    E::Call { callee, args } => (callee.clone(), std::mem::take(args)),
                    E::New { args, .. } => (
                        Callee::Intrinsic(Intrinsic::PromiseWiden),
                        std::mem::take(args),
                    ),
                    _ => return,
                };
                self.call(&probe, &callee, &mut args);
                if let E::Call { args: a, .. } | E::New { args: a, .. } = &mut e.kind {
                    *a = args;
                }
            }
            _ => {}
        }
    }
}
