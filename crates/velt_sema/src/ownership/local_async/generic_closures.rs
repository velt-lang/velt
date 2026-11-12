//! The instantiations of generic closures (#2860). A generic closure's value is an interface value
//! of its generic function type (hir_encodings.md "Generic function values"), and every call of it
//! is a `Callee::Dyn` call of that interface with the call's type arguments. Those calls, recorded
//! by the graph's walk, are the closure's instantiations, as direct calls are a generic function's
//! (`Graph::insts`), so `super::instances` can resolve a type the closure's type parameters stand
//! in for instead of leaving it generic, where a crossing value of it could be any closure.
//!
//! Only where the walk sees every call:
//! - the closure's enclosing function isn't generic (or there is none), so the call's type
//!   arguments are all of its instantiation;
//! - no value of the interface type goes into `any` (`ToAny`, a spread) or is converted to another
//!   type (a cast, an upcast), from where a call the walk doesn't see could reach it;
//! - no code outside every function body (module-level and field initializers, which the walk
//!   doesn't visit) calls a value of the interface type or converts one.
//!
//! Otherwise the closure keeps no instantiation and its types stay generic, as before.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::Ctx;
use crate::defs::{DefInfo, FnKind};
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, TyId, TyKind};
use crate::ownership::local_closures::walk::{self, Visit};

use super::graph::Graph;

/// A call giving a generic function its type arguments: the function making it, and the types.
pub(super) type Site = (DefId, Vec<TyId>);

/// What the walk saw of generic function values' interfaces.
#[derive(Default)]
pub(super) struct DynCalls {
    /// Per interface and method slot, each `Dyn` call's function and type arguments.
    calls: FxHashMap<(DefId, u32), Vec<Site>>,
    /// Interfaces some value of which is called where the walk may not see it.
    escaped: FxHashSet<DefId>,
}

impl DynCalls {
    /// A `Dyn` call of method `slot` in function `caller`, of receiver type `recv`, with `targs`.
    pub(super) fn call(&mut self, cx: &Ctx, caller: DefId, recv: TyId, slot: u32, targs: &[TyId]) {
        if let TyKind::Dyn(i, _) = cx.ty.kind(recv) {
            (self.calls.entry((*i, slot)).or_default()).push((caller, targs.to_vec()));
        }
    }

    /// A value of type `ty` goes into `any` or is converted to another type: the interfaces it
    /// mentions escape, also through the fields of the objects it holds (a `(engine as any).run(…)`
    /// call reaches a generic function value in a field).
    pub(super) fn escape(&mut self, cx: &Ctx, ty: TyId) {
        let mut seen = FxHashSet::default();
        let mut work = vec![ty];
        while let Some(t) = work.pop() {
            if !seen.insert(t) {
                continue;
            }
            cx.ty.visit(t, &mut |k| match k {
                TyKind::Dyn(i, _) => _ = self.escaped.insert(*i),
                TyKind::Adt(d, _) => {
                    if let Some(Def::Adt(a)) = &cx.defs[d.0 as usize] {
                        work.extend(a.fields.iter().map(|f| f.ty));
                    }
                }
                _ => {}
            });
        }
    }

    /// Code outside every function body: the interfaces any of its expressions mentions escape.
    pub(super) fn finish(&mut self, cx: &Ctx) {
        let mut tys = vec![];
        let mut outside = Outside(&mut tys);
        for info in cx.info.iter() {
            match info {
                DefInfo::Global(gi) => {
                    if let Some(init) = &gi.init {
                        walk::expr(init, &mut outside);
                    }
                }
                DefInfo::Adt(a) => {
                    for v in a.fields.iter().filter_map(|f| f.default.as_ref()) {
                        walk::expr(v, &mut outside);
                    }
                    for v in a.redefined.iter().filter_map(|r| r.init.as_ref()) {
                        walk::expr(v, &mut outside);
                    }
                }
                _ => {}
            }
        }
        for def in cx.defs.iter().flatten() {
            match def {
                Def::Global(gd) => walk::expr(&gd.init, &mut outside),
                Def::Adt(a) => {
                    for v in a.fields.iter().filter_map(|f| f.default.as_ref()) {
                        walk::expr(v, &mut outside);
                    }
                    for init in &a.inherited_inits {
                        walk::expr(&init.init, &mut outside);
                    }
                }
                _ => {}
            }
        }
        for t in tys {
            self.escape(cx, t);
        }
    }

    /// The instantiations a generic closure, or a generic method of an implementation, gets from
    /// the `Dyn` calls of its interface, when the walk sees every call of it (module docs): each
    /// call of its slot, with the function making it. `None` when `f` is neither, its type
    /// parameters aren't all its own (an enclosing function's, a generic implementing type's), or
    /// a call may be unseen.
    pub(super) fn sites(&self, cx: &Ctx, g: &Graph, f: DefId) -> Option<Vec<Site>> {
        let Some(Def::Fn(fd)) = &cx.defs[f.0 as usize] else {
            return None;
        };
        let kind = cx.try_fn(f).map(|i| i.kind);
        if fd.generics == 0 || !matches!(kind, Some(FnKind::Closure | FnKind::Method)) {
            return None;
        }
        if kind == Some(FnKind::Closure) {
            // Its own type parameters only: an enclosing function's would need that function's
            // instantiation too.
            let enclosing = g
                .parent
                .get(&f)
                .map_or(0, |p| match &cx.defs[p.0 as usize] {
                    Some(Def::Fn(pf)) => pf.generics,
                    _ => 0,
                });
            if enclosing != 0 {
                return None;
            }
        }
        let mut slots = vec![];
        for i in &cx.impls {
            if let Some(k) = i.methods.iter().position(|&m| m == f) {
                if i.generics != 0 {
                    return None;
                }
                slots.push((i.iface, k as u32));
            }
        }
        if slots.is_empty() || slots.iter().any(|(i, _)| self.escaped.contains(i)) {
            return None;
        }
        Some(
            slots
                .iter()
                .flat_map(|s| self.calls.get(s).into_iter().flatten().cloned())
                .collect(),
        )
    }
}

/// Collects, from code outside every function body, the types whose generic function values it
/// may call where the walk doesn't see it: the receivers of its `Dyn` calls and what it converts.
struct Outside<'a>(&'a mut Vec<TyId>);

impl Visit for Outside<'_> {
    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            E::Call {
                callee: Callee::Dyn { .. },
                args,
            } => self.0.extend(args.first().map(|r| r.ty)),
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::ToAny | Intrinsic::SpreadObject),
                args,
            } => self.0.extend(args.iter().map(|a| a.ty)),
            E::Cast(x) | E::Upcast(x) | E::Downcast(x) => self.0.push(x.ty),
            _ => {}
        }
    }
}
