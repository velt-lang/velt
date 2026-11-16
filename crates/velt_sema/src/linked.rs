//! The modules a program links in a check by units (`crate::units`): those its root reaches,
//! through imports and through the definitions their code names. A whole-program check holds
//! only what the program uses (std is loaded on demand, docs/internals/design/lazy-std.md), so
//! it links every module it holds. A check by units holds every module of a finished unit, all
//! of std, and only the modules the program reaches exist when it runs: only their module state
//! is initialized and checked (`module_state::order`).
//!
//! A module's edges are the modules it imports and those of the definitions its functions and
//! its constants' and fields' initializers name or handle: a call, a constant read, a function
//! taken as a value, `new`, a literal, and the types of its expressions (a value of a class may
//! run the class's methods, its hooks too). That links more than runs, never less: a module left
//! out would have state the program uses and never initializes.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, TyId, TyKind};
use crate::units::Stage;

/// A finished unit's edges (`Ctx::module_edges`): per module of the unit, the modules its code
/// names, imports aside.
#[derive(Clone, Default)]
pub(crate) struct ModuleEdges(FxHashMap<usize, FxHashSet<usize>>);

/// Per module, whether the program links it (module docs): every module outside a check by
/// units and in std's stage, which has no program. std's stage keeps its modules' edges for the
/// app's.
pub(crate) fn linked(cx: &Ctx) -> Vec<bool> {
    let all = vec![true; cx.modules.len()];
    match (cx.units, cx.units_stage) {
        (false, _) | (true, Stage::Std) => all,
        (true, Stage::Both) => reach(cx, &named(cx, 0)),
        (true, Stage::App) => {
            let mut edges = cx.unit_sums.module_edges.0.clone();
            edges.extend(named(cx, cx.unit_base as usize).0);
            reach(cx, &ModuleEdges(edges))
        }
    }
}

/// std's stage: its modules' edges, for the app's [`linked`].
pub(crate) fn keep_edges(cx: &mut Ctx) {
    if cx.units && cx.units_stage == Stage::Std {
        cx.unit_sums.module_edges = named(cx, 0);
    }
}

/// The modules reached from the root over imports and `edges`.
fn reach(cx: &Ctx, edges: &ModuleEdges) -> Vec<bool> {
    let by_path: FxHashMap<&str, usize> = (cx.modules.iter().enumerate())
        .map(|(i, m)| (m.path.as_str(), i))
        .collect();
    let mut out = vec![false; cx.modules.len()];
    let mut todo = vec![cx.root];
    out[cx.root] = true;
    while let Some(m) = todo.pop() {
        let imports = (cx.modules[m].imports.iter()).filter_map(|(_, p)| by_path.get(p.as_str()));
        let named = edges.0.get(&m).into_iter().flatten();
        for &dep in imports.chain(named) {
            if !out[dep] {
                out[dep] = true;
                todo.push(dep);
            }
        }
    }
    out
}

/// The modules each module's code names, of the definitions from `from` on (module docs).
fn named(cx: &Ctx, from: usize) -> ModuleEdges {
    let mut names = Names {
        cx,
        defs: FxHashSet::default(),
        tys: FxHashSet::default(),
    };
    let mut per_module: FxHashMap<usize, (FxHashSet<DefId>, FxHashSet<TyId>)> =
        FxHashMap::default();
    let mut add = |m: usize, names: &mut Names| {
        let e = per_module.entry(m).or_default();
        e.0.extend(names.defs.drain());
        e.1.extend(names.tys.drain());
    };
    for (i, d) in cx.defs.iter().enumerate().skip(from) {
        let Some(Def::Fn(f)) = d else { continue };
        let Some(DefInfo::Fn(fi)) = cx.info.get(i) else {
            continue;
        };
        names.tys.extend(f.body.locals.iter().map(|l| l.ty));
        crate::visit_ref::block(&f.body.block, &mut names);
        add(fi.module, &mut names);
    }
    for i in from..cx.info.len() {
        let (module, inits): (usize, Vec<&Expr>) = match &cx.info[i] {
            DefInfo::Global(g) => (g.module, g.init.iter().collect()),
            DefInfo::Adt(a) => (
                a.module,
                (a.fields.iter().filter_map(|f| f.default.as_ref()))
                    .chain(a.redefined.iter().filter_map(|r| r.init.as_ref()))
                    .collect(),
            ),
            _ => continue,
        };
        for x in inits {
            crate::visit_ref::expr(x, &mut names);
        }
        add(module, &mut names);
    }
    let mut out = ModuleEdges::default();
    for (m, (defs, tys)) in per_module {
        let mut mods: FxHashSet<usize> = defs.iter().filter_map(|d| module_of(cx, *d)).collect();
        let mut seen = FxHashSet::default();
        for t in tys {
            type_modules(cx, t, &mut seen, &mut mods);
        }
        mods.remove(&m);
        if !mods.is_empty() {
            out.0.insert(m, mods);
        }
    }
    out
}

/// The module of definition `d`.
fn module_of(cx: &Ctx, d: DefId) -> Option<usize> {
    Some(match cx.info.get(d.0 as usize)? {
        DefInfo::Fn(f) => f.module,
        DefInfo::Adt(a) => a.module,
        // An enum names no module of its own: its file's (none for a union's).
        DefInfo::Enum(e) if e.is_union => return None,
        DefInfo::Enum(e) => cx.modules.iter().position(|m| m.file == e.span.file)?,
        DefInfo::Iface(i) => i.module,
        DefInfo::Global(g) => g.module,
    })
}

/// Add the modules of the classes, enums, interfaces and closures type `t` names, deeply.
fn type_modules(cx: &Ctx, t: TyId, seen: &mut FxHashSet<TyId>, out: &mut FxHashSet<usize>) {
    if !seen.insert(t) {
        return;
    }
    let k = cx.ty.kind(t);
    if let TyKind::Adt(d, _) | TyKind::Dyn(d, _) | TyKind::Closure(d) = k {
        out.extend(module_of(cx, *d));
    }
    for c in crate::types::children(k) {
        type_modules(cx, c, seen, out);
    }
}

/// The definitions and types one body names.
struct Names<'a, 'm> {
    cx: &'a Ctx<'m>,
    defs: FxHashSet<DefId>,
    tys: FxHashSet<TyId>,
}

impl crate::visit_ref::Visit for Names<'_, '_> {
    fn expr(&mut self, e: &Expr) {
        self.tys.insert(e.ty);
        match &e.kind {
            E::Global(d) | E::FnRef(d, _) | E::Closure(d) => {
                self.defs.insert(*d);
            }
            E::New { def, .. } | E::AdtLit { def, .. } | E::Variant { def, .. } => {
                self.defs.insert(*def);
                if let Some(c) = self.cx.adt(*def).and_then(|a| a.ctor) {
                    self.defs.insert(c);
                }
            }
            E::Call {
                callee: Callee::Def(f, _),
                ..
            } => {
                self.defs.insert(*f);
            }
            E::ToDyn { impl_index, .. } => {
                if let Some(imp) = self.cx.impls.get(*impl_index as usize) {
                    self.defs.insert(imp.iface);
                    self.defs.extend(imp.methods.iter().copied());
                }
            }
            _ => {}
        }
    }
}

// The encoding of a finished unit (`units::codec`): every field, so a new one fails to build.
crate::units::codec::tuples! {
    ModuleEdges(f0)
}
