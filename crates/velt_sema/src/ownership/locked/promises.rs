//! Promises made by a `with` callback from the locked value (super module docs): a call or
//! `new` whose result holds a promise and whose arguments (the receiver included) may reach
//! the locked value (`super::regions`). It runs after the lock is released, so it would use
//! the value without the lock. Calling a function value taken from the value is allowed
//! (`m.with((f) => f())`): an async closure copies what it captured per call. `spawn`
//! transfers what it is given, so a spawned call is allowed too.

use std::collections::HashSet;

use velt_common::Span;

use super::regions::{Regions, IN};
use crate::ctx::Ctx;
use crate::hir::{Callee, DefId, Expr, ExprKind as E, FnDef, Intrinsic, TyId};
use crate::visit::{self, VisitMut};

/// The promise-making expressions of body `f` (of `def`) that use the locked value, with
/// their types.
pub(super) fn made_from_value(
    cx: &mut Ctx,
    r: &Regions,
    def: DefId,
    f: &mut FnDef,
) -> Vec<(Span, TyId)> {
    let mut p = Promises {
        cx,
        r,
        def,
        spawned: HashSet::new(),
        found: vec![],
    };
    visit::block(&mut f.body.block, &mut p);
    p.found
}

struct Promises<'a, 'r, 'm> {
    cx: &'a mut Ctx<'m>,
    r: &'r Regions,
    def: DefId,
    /// Spans of the calls `spawn` takes.
    spawned: HashSet<Span>,
    found: Vec<(Span, TyId)>,
}

impl Promises<'_, '_, '_> {
    /// Do the inputs of a promise-making expression reach the locked value?
    fn uses_value(&mut self, args: &[Expr]) -> bool {
        args.iter()
            .any(|a| self.r.mentions(self.cx, self.def, a) & IN != 0)
    }
}

impl VisitMut for Promises<'_, '_, '_> {
    fn expr(&mut self, e: &mut Expr) {
        let args = match &e.kind {
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::Spawn),
                args,
            } => {
                self.spawned.extend(args.iter().map(|a| a.span));
                return;
            }
            // Widens a promise made in its argument (found there).
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::PromiseWiden),
                ..
            } => return,
            E::Call { args, .. } | E::New { args, .. } => args,
            _ => return,
        };
        if self.spawned.contains(&e.span) || !self.cx.holds_promise(e.ty) {
            return;
        }
        let args = args.clone();
        if self.uses_value(&args) {
            self.found.push((e.span, e.ty));
        }
    }
}
