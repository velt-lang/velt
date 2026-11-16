//! The copies each function body makes (`Share` / `Clone` intrinsics) and its calls of generic
//! functions, collected after all bodies and the ownership passes (which add shares), for the
//! fixed points over the call graph that need them (`crate::weak_threads`). Promises used to be
//! checked here (they had one owner); since #212 a copy of a promise shares it.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::Span;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, StmtKind, TyId};
use crate::inst_sites::guards_of;

/// A copied type and whether the copy is deep (a clone copies arrays and objects too; a share
/// only copies values in place and adds a reference to counted objects).
type Copy = (TyId, bool);

/// The facts of one function body.
#[derive(Clone, Default)]
pub(crate) struct Facts {
    copies: Vec<Copy>,
    /// Generic calls: callee, type arguments, span of the call.
    pub(crate) calls: Vec<(DefId, Vec<TyId>, Span)>,
    /// The guard bits of each of `calls` (`inst_sites::carry_guard`).
    pub(crate) guards: Vec<u64>,
    pub(crate) closures: Vec<DefId>,
    /// Class values whose methods are called dynamically (virtual calls, interface values).
    pub(crate) dispatch: Vec<(TyId, Span)>,
    /// The methods those reach, with the class's type arguments.
    pub(crate) dyn_calls: Vec<(DefId, Vec<TyId>, Span)>,
    /// Spans of `for...of` iterables (a call there is "iterating").
    loops: FxHashSet<Span>,
}

struct Collect<'a, 'm> {
    facts: &'a mut Facts,
    /// The definitions: a class's constructor, and whether a function is one (`super(...)`
    /// passes `this` upcast to one), are read from them as calls reach them.
    info: &'a crate::ctx::def_table::DefTable<DefInfo<'m>>,
    /// Spans of `this` upcast for a base constructor (`super(...)`), which is not a use of the
    /// subclass as its base: counting it would give every subclass constructor its overrides'
    /// copies.
    super_receivers: FxHashSet<Span>,
    /// Guard bits per expression (`inst_sites::guards_of`).
    guards: FxHashMap<*const Expr, u64>,
}

impl Collect<'_, '_> {
    fn call(&mut self, e: &Expr, d: DefId, targs: Vec<TyId>) {
        self.facts.calls.push((d, targs, e.span));
        let guard = self.guards.get(&(e as *const Expr)).copied().unwrap_or(0);
        self.facts.guards.push(guard);
    }
}

impl crate::visit_ref::Visit for Collect<'_, '_> {
    fn stmt(&mut self, s: &crate::hir::Stmt) {
        if let StmtKind::ForOf { iter, .. } = &s.kind {
            self.facts.loops.insert(iter.span);
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            E::Call {
                callee: Callee::Intrinsic(i @ (Intrinsic::Share | Intrinsic::Clone)),
                args,
            } => {
                if let Some(a) = args.first() {
                    self.facts.copies.push((a.ty, *i == Intrinsic::Clone));
                }
            }
            E::Call {
                callee: Callee::Def(d, targs),
                args,
            } => {
                let ctor = matches!(self.info.get(d.0 as usize), Some(DefInfo::Fn(f)) if f.kind == crate::defs::FnKind::Ctor);
                if ctor {
                    if let Some(
                        recv @ Expr {
                            kind: E::Upcast(_), ..
                        },
                    ) = args.first()
                    {
                        self.super_receivers.insert(recv.span);
                    }
                }
                if !targs.is_empty() {
                    let (d, targs) = (*d, targs.clone());
                    self.call(e, d, targs);
                }
            }
            // A constructor copies from its arguments (`new Map(entries)`); without any (only
            // empty array literals) there is nothing to copy.
            E::New {
                def,
                type_args,
                args,
            } if !type_args.is_empty() && !args.iter().all(is_empty_array) => {
                let ctor = match self.info.get(def.0 as usize) {
                    Some(DefInfo::Adt(a)) => a.ctor,
                    _ => None,
                };
                if let Some(ctor) = ctor {
                    let targs = type_args.clone();
                    self.call(e, ctor, targs);
                }
            }
            E::Closure(c) => self.facts.closures.push(*c),
            // A generic function used as a value: its copies happen wherever it is called.
            E::FnRef(d, targs) if !targs.is_empty() => {
                let (d, targs) = (*d, targs.clone());
                self.call(e, d, targs);
            }
            // Dynamic dispatch: the methods a class value's vtable can reach.
            E::ToDyn { expr, .. } => self.facts.dispatch.push((expr.ty, e.span)),
            // A subclass instance used as its base: virtual calls on the base reach the
            // subclass's overrides, which the receiver's (base) type doesn't list.
            E::Upcast(inner) if !self.super_receivers.contains(&e.span) => {
                self.facts.dispatch.push((inner.ty, e.span));
            }
            E::Call {
                callee: Callee::Virtual { .. },
                args,
            } => {
                if let Some(recv) = args.first() {
                    self.facts.dispatch.push((recv.ty, e.span));
                }
            }
            _ => {}
        }
    }
}

/// `[]` (a defaulted `entries` argument): holds nothing to copy.
fn is_empty_array(e: &Expr) -> bool {
    matches!(&e.kind, E::ArrayLit(xs) if xs.is_empty())
}

/// The facts of every function body, in definition order (also `crate::weak_threads`).
/// The facts of each function body from definition `from` on.
pub(crate) fn collect(cx: &mut Ctx, from: usize) -> Vec<(DefId, Facts)> {
    let mut out = vec![];
    for (i, d) in cx.defs.iter().enumerate().skip(from) {
        let Some(Def::Fn(f)) = d else { continue };
        let mut facts = Facts::default();
        let guards = guards_of(&cx.ty, &f.body.block);
        // A capture that shares the enclosing variable copies it, without an intrinsic.
        for c in &f.captures {
            if c.share {
                facts
                    .copies
                    .push((f.body.locals[c.inner.0 as usize].ty, false));
            }
        }
        crate::visit_ref::block(
            &f.body.block,
            &mut Collect {
                facts: &mut facts,
                info: &cx.info,
                super_receivers: FxHashSet::default(),
                guards,
            },
        );
        out.push((DefId(i as u32), facts));
    }
    out
}

// The encoding of a finished unit (`units::codec`): every field, so a new one fails to build.
crate::units::codec::structs! {
    Facts {
        copies, calls, guards, closures, dispatch, dyn_calls, loops,
    }
}
