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
use crate::visit::{self, VisitMut};

/// A copied type and whether the copy is deep (a clone copies arrays and objects too; a share
/// only copies values in place and adds a reference to counted objects).
type Copy = (TyId, bool);

/// The facts of one function body.
#[derive(Default)]
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

struct Collect<'a> {
    facts: &'a mut Facts,
    ctors: &'a FxHashMap<DefId, DefId>,
    /// Constructors: `super(...)` passes `this` upcast to one.
    ctor_fns: &'a FxHashSet<DefId>,
    /// Spans of `this` upcast for a base constructor (`super(...)`), which is not a use of the
    /// subclass as its base: counting it would give every subclass constructor its overrides'
    /// copies.
    super_receivers: FxHashSet<Span>,
    /// Guard bits per expression (`inst_sites::guards_of`).
    guards: FxHashMap<*const Expr, u64>,
}

impl Collect<'_> {
    fn call(&mut self, e: &Expr, d: DefId, targs: Vec<TyId>) {
        self.facts.calls.push((d, targs, e.span));
        let guard = self.guards.get(&(e as *const Expr)).copied().unwrap_or(0);
        self.facts.guards.push(guard);
    }
}

impl VisitMut for Collect<'_> {
    fn stmt(&mut self, s: &mut crate::hir::Stmt) {
        if let StmtKind::ForOf { iter, .. } = &s.kind {
            self.facts.loops.insert(iter.span);
        }
    }

    fn expr(&mut self, e: &mut Expr) {
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
                if self.ctor_fns.contains(d) {
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
                if let Some(&ctor) = self.ctors.get(def) {
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
pub(crate) fn collect(cx: &mut Ctx) -> Vec<(DefId, Facts)> {
    let ctors: FxHashMap<DefId, DefId> = (0..cx.info.len())
        .filter_map(|i| match &cx.info[i] {
            DefInfo::Adt(a) => a.ctor.map(|c| (DefId(i as u32), c)),
            _ => None,
        })
        .collect();
    let ctor_fns: FxHashSet<DefId> = ctors.values().copied().collect();
    let mut out = vec![];
    for (i, d) in cx.defs.iter_mut().enumerate() {
        let Some(Def::Fn(f)) = d else { continue };
        let mut facts = Facts::default();
        let guards = guards_of(&cx.ty, &mut f.body.block);
        // A capture that shares the enclosing variable copies it, without an intrinsic.
        for c in &f.captures {
            if c.share {
                facts
                    .copies
                    .push((f.body.locals[c.inner.0 as usize].ty, false));
            }
        }
        visit::block(
            &mut f.body.block,
            &mut Collect {
                facts: &mut facts,
                ctors: &ctors,
                ctor_fns: &ctor_fns,
                super_receivers: FxHashSet::default(),
                guards,
            },
        );
        out.push((DefId(i as u32), facts));
    }
    out
}
