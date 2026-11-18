//! `x.m = () => { … x … }`: a method written through an interface value
//! (`crate::iface_method_writes`) with a function that captures the receiver, the usual shape of
//! such a write (`ctx.update = () => render(ctx)`).
//!
//! The write is a call of the method's setter slot `<set m()>` with `[x, f]`, `x` borrowed
//! mutably. A setter of that slot is always one sema makes: it stores the function in the
//! object's field, or converts it and calls the wrapped value's setter of the slot (an adapter),
//! and runs none of the program's code while `x` is borrowed. So the closure may hold a share of
//! the object `x` refers to, as JavaScript's closure refers to it: its capture of `x` becomes a
//! share, and `x` is only read by the call. The closure is stored, not called, so what it does
//! to the object when it runs later (`x.count += 1`) can't happen under the call's borrow. A
//! variable that is assigned while a closure sees it (`LocalDef::boxed`) keeps the usual rule.
//!
//! Field setters keep the strict rule: an implementation may fill one with the program's own
//! `set f(v)` accessor.

use crate::ctx::Ctx;
use crate::hir::{Callee, Expr, TyKind};

use super::conflict::first_conflict;
use super::uses::Access;
use super::{alias_root, ArgUses, Checker};

/// Is `callee` (called with `args`) a method setter slot of an interface whose methods are
/// written?
fn is_method_setter(cx: &Ctx, callee: &Callee, args: &[Expr]) -> bool {
    let (iface, slot) = match callee {
        Callee::Dyn { slot, .. } => match args.first().map(|a| cx.ty.kind(a.ty)) {
            Some(TyKind::Dyn(d, _)) => (*d, *slot),
            _ => return false,
        },
        Callee::ParamMethod { iface, slot, .. } => (*iface, *slot),
        _ => return false,
    };
    let Some(i) = cx.iface(iface) else {
        return false;
    };
    let first = i.methods.len() + 2 * i.fields.len();
    cx.methods_written(iface) && (first..first + i.methods.len()).contains(&(slot as usize))
}

impl Checker<'_, '_> {
    /// A method setter call whose function captures the receiver (`uses[first]` the receiver's,
    /// `uses[first + 1]` the function's): checked with that capture as a share and the receiver
    /// read. False when the call is not one (the usual check applies).
    pub(super) fn method_setter_call(
        &mut self,
        uses: &mut ArgUses,
        first: usize,
        args: &[Expr],
        callee: Option<&Callee>,
    ) -> bool {
        let setter = callee.is_some_and(|c| is_method_setter(self.cx, c, args));
        if !setter || args.len() != 2 || uses.len() != first + 2 {
            return false;
        }
        let Some(root) = alias_root(&args[0]) else {
            return false;
        };
        // A variable assigned while a closure sees it lives in a cell (`LocalDef::boxed`): the
        // usual rule.
        if self.locals.get(root.0 as usize).is_none_or(|l| l.boxed) {
            return false;
        }
        let mut caps = vec![];
        for u in &mut uses[first + 1].0 {
            let Some((c, outer)) = u.capture else {
                continue;
            };
            let takes = matches!(u.access, Access::Move | Access::Unique);
            if outer == root && u.place.root == root && takes {
                u.access = Access::Shared;
                caps.push((c, outer));
            }
        }
        if caps.is_empty() {
            return false;
        }
        for u in &mut uses[first].0 {
            if u.place.root == root && !u.captured {
                u.access = Access::Shared;
            }
        }
        match first_conflict(uses, &|_, _| false) {
            Some(d) => self.errors.push(d),
            None => (caps.into_iter()).for_each(|(c, outer)| self.shares.push((c, outer, None))),
        }
        true
    }
}
