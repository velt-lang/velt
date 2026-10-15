//! Module state (#76): module-level `let`s become slots of one process-wide tuple
//! (hir_encodings.md "Module state"). Slots are numbered in Node's module evaluation order: a
//! depth-first walk over each module's imports in source order, a module after the modules it
//! imports, and its own declarations in source order. The state's initializer evaluates them
//! in that order, so an initializer that reads a `let` whose initializer has not run yet is an
//! error, as Node throws a `ReferenceError` there.

use std::collections::HashMap;

use velt_common::Diagnostic;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{self, DefId, ExprKind as E, TyKind};

/// The module-level `let`s in initialization order (empty when there are none); reports reads
/// of a `let` before its initializer runs.
pub(crate) fn order(cx: &mut Ctx) -> Vec<DefId> {
    let lets: Vec<(usize, u32, DefId)> = cx
        .info
        .iter()
        .enumerate()
        .filter_map(|(i, d)| match d {
            DefInfo::Global(g) if g.stored => Some((g.module, g.src.span.lo, DefId(i as u32))),
            _ => None,
        })
        .collect();
    if lets.is_empty() {
        return Vec::new();
    }
    let modules = module_order(cx);
    let rank: HashMap<usize, usize> = modules.iter().enumerate().map(|(i, m)| (*m, i)).collect();
    let mut sorted = lets;
    sorted.sort_by_key(|(m, start, _)| (rank.get(m).copied().unwrap_or(usize::MAX), *start));
    let order: Vec<DefId> = sorted.into_iter().map(|(_, _, d)| d).collect();
    let pos: HashMap<DefId, usize> = order.iter().enumerate().map(|(i, d)| (*d, i)).collect();
    for (i, &d) in order.iter().enumerate() {
        let Some(g) = cx.global(d) else { continue };
        let Some(init) = g.init.as_ref() else {
            continue;
        };
        let name = g.name.clone();
        let mut early = Vec::new();
        reads(init, &mut |r, span| {
            if pos.get(&r).is_some_and(|&j| j >= i) {
                early.push((r, span));
            }
        });
        for (r, span) in early {
            let other = cx.global(r).map(|g| g.name.clone()).unwrap_or_default();
            cx.error(
                Diagnostic::error(
                    format!("`{other}` is read before its initializer runs"),
                    span,
                )
                .with_note(format!(
                    "module-level `let`s are initialized in order, and `{name}`'s initializer \
                     runs first (JavaScript throws a `ReferenceError` here)"
                )),
            );
        }
    }
    order
}

/// Module indices in evaluation order: those reachable from the root by a post-order walk over
/// imports, then any others (which have no code that runs first).
fn module_order(cx: &Ctx) -> Vec<usize> {
    let by_path: HashMap<&str, usize> = cx
        .modules
        .iter()
        .enumerate()
        .map(|(i, m)| (m.path.as_str(), i))
        .collect();
    let mut seen = vec![false; cx.modules.len()];
    let mut out = Vec::new();
    // Iterative post-order: (module, next import to visit).
    let mut stack = vec![(cx.root, 0usize)];
    seen[cx.root] = true;
    while let Some((m, next)) = stack.pop() {
        let imports = &cx.modules[m].imports;
        if let Some((_, path)) = imports.get(next) {
            stack.push((m, next + 1));
            if let Some(&dep) = by_path.get(path.as_str()) {
                if !seen[dep] {
                    seen[dep] = true;
                    stack.push((dep, 0));
                }
            }
        } else {
            out.push(m);
        }
    }
    let root = out.pop();
    for (m, s) in seen.iter().enumerate() {
        if !s {
            out.push(m);
        }
    }
    out.extend(root);
    out
}

/// Every module global `e` reads directly.
fn reads(e: &hir::Expr, f: &mut dyn FnMut(DefId, velt_common::Span)) {
    let mut e = e.clone();
    struct V<'a>(&'a mut dyn FnMut(DefId, velt_common::Span));
    impl crate::visit::VisitMut for V<'_> {
        fn expr(&mut self, e: &mut hir::Expr) {
            if let E::Global(d) = e.kind {
                (self.0)(d, e.span);
            }
        }
    }
    crate::visit::expr(&mut e, &mut V(f));
}

/// The program's module state, with each slot's global numbered (`GlobalDef::slot`).
pub(crate) fn build(cx: &mut Ctx) -> Option<hir::ModuleState> {
    if cx.module_slots.is_empty() {
        return None;
    }
    let slots = cx.module_slots.clone();
    let mut tys = Vec::with_capacity(slots.len());
    for (i, &d) in slots.iter().enumerate() {
        let Some(Some(hir::Def::Global(g))) = cx.defs.get_mut(d.0 as usize) else {
            panic!("ICE: module state slot is not a global");
        };
        g.slot = Some(i as u32);
        tys.push(g.ty);
    }
    let ty = cx.ty.intern(TyKind::Tuple(tys));
    Some(hir::ModuleState { ty, slots })
}

impl crate::body::FnCx<'_, '_> {
    /// `x = v` where `x` names a module-level `let` (or a kept `const`, which `require_mutable`
    /// rejects): its slot, or `Some(None)` after an error; `None` for any other name.
    pub(crate) fn module_let_target(
        &mut self,
        item: Option<crate::ctx::Item>,
        id: &velt_syntax::ast::Ident,
    ) -> Option<Option<hir::Expr>> {
        let Some(crate::ctx::Item::Def(d)) = item else {
            return None;
        };
        if !self.cx.global(d).is_some_and(|g| g.stored) {
            return None;
        }
        let place = self.global_read(d, crate::body::Want::BorrowMut, id.span);
        Some(self.require_mutable(&place, "assign to").then_some(place))
    }
}

/// Why a function may reach module state: the `let` or kept `const` it uses, or that it calls a
/// function value whose body is not known; with the calls that lead there.
#[derive(Clone)]
struct Reach {
    what: String,
    via: Vec<DefId>,
}

/// What one body does with module state: its own first use, and the functions it may run (calls,
/// closures it makes, functions it takes as values, the methods a dynamic call may pick).
struct Facts {
    direct: Option<String>,
    edges: Vec<DefId>,
}

/// A `Mutex` is locked by blocking the thread, while a task holds the module lock for its whole
/// step: a `with` callback that reaches module state could wait for the module lock while holding
/// the `Mutex` another thread waits for, holding the module lock. So module state is not
/// reachable from a `with` callback: not directly, not through the functions it calls, and not
/// through a function value whose body is not known (rt_abi.md "Module state"; the runtime stops
/// with an `ICE:` if it happens anyway).
pub(crate) fn check_locked_access(cx: &mut Ctx) {
    if cx.module_slots.is_empty() {
        return;
    }
    let fns: Vec<DefId> = cx
        .fn_defs
        .iter()
        .copied()
        .filter(|d| cx.fn_info(*d).state == crate::defs::BodyState::Done)
        .collect();
    let mut withs: Vec<(hir::Expr, velt_common::Span)> = vec![];
    let mut facts: HashMap<DefId, Facts> = HashMap::new();
    for &d in &fns {
        let std = cx.modules[cx.fn_info(d).module].is_std;
        let Some(hir::Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        let mut v = FactsOf {
            cx,
            std,
            facts: Facts {
                direct: None,
                edges: vec![],
            },
            withs: vec![],
        };
        crate::visit::block(&mut f.body.block, &mut v);
        let (fd, ws) = (v.facts, v.withs);
        cx.defs[d.0 as usize] = Some(hir::Def::Fn(f));
        if !std {
            withs.extend(ws);
        }
        facts.insert(d, fd);
    }
    if withs.is_empty() {
        return;
    }
    let reach = propagate(&facts);
    for (cb, span) in withs {
        let found = match &cb.kind {
            E::Closure(c) => reach.get(c).cloned(),
            E::FnRef(c, _) => reach.get(c).cloned().map(|mut r| {
                r.via.insert(0, *c);
                r
            }),
            _ => Some(Reach {
                what: "is a function value whose body is not known here".into(),
                via: vec![],
            }),
        };
        let Some(r) = found else { continue };
        let what = match r.via.is_empty() {
            true => r.what,
            false => {
                let chain: Vec<String> = r
                    .via
                    .iter()
                    .map(|&f| {
                        let n = &cx.fn_info(f).name;
                        format!("`{}`", n.rsplit("::").next().unwrap_or(n))
                    })
                    .collect();
                format!("{} (through {})", r.what, chain.join(" -> "))
            }
        };
        cx.error(
            Diagnostic::error(
                format!("the function passed to `with` may use module state: it {what}"),
                span,
            )
            .with_note("`with` blocks the thread while it waits for the lock, so module-level `let`s and kept constants cannot be used under it: two tasks could wait for each other")
            .with_note("read what you need before `with` and pass it in, and store what it returns after"),
        );
    }
}

/// Which functions may reach module state: those that use it, and those that may run one of
/// them (backwards along the edges, to a fixpoint).
fn propagate(facts: &HashMap<DefId, Facts>) -> HashMap<DefId, Reach> {
    let mut reach: HashMap<DefId, Reach> = HashMap::new();
    let mut callers: HashMap<DefId, Vec<DefId>> = HashMap::new();
    let mut todo = vec![];
    for (&d, f) in facts {
        for &e in &f.edges {
            callers.entry(e).or_default().push(d);
        }
        if let Some(what) = &f.direct {
            let r = Reach {
                what: what.clone(),
                via: vec![],
            };
            reach.insert(d, r);
            todo.push(d);
        }
    }
    while let Some(d) = todo.pop() {
        let r = reach[&d].clone();
        for &c in callers.get(&d).map(Vec::as_slice).unwrap_or(&[]) {
            if reach.contains_key(&c) {
                continue;
            }
            let mut via = vec![d];
            via.extend(r.via.iter().copied());
            let what = r.what.clone();
            reach.insert(c, Reach { what, via });
            todo.push(c);
        }
    }
    reach
}

/// Collects a body's [`Facts`] and the callbacks of its `with` calls.
struct FactsOf<'a, 'm> {
    cx: &'a Ctx<'m>,
    std: bool,
    facts: Facts,
    withs: Vec<(hir::Expr, velt_common::Span)>,
}

impl FactsOf<'_, '_> {
    fn unknown(&mut self) {
        // The standard library only calls the function values it is given: those are made, or
        // taken as values, by the code that passes them, where they count.
        if !self.std && self.facts.direct.is_none() {
            self.facts.direct = Some("calls a function value".into());
        }
    }

    /// The methods a call through slot `slot` of interface `iface` may run.
    fn iface_methods(&mut self, iface: DefId, slot: u32) {
        for imp in &self.cx.impls {
            if imp.iface == iface {
                self.facts.edges.extend(imp.methods.get(slot as usize));
            }
        }
    }

    /// The methods a virtual call through slot `slot` of class `class` may run: its own and every
    /// subclass's.
    fn virtual_methods(&mut self, class: DefId, slot: u32) {
        for (i, info) in self.cx.info.iter().enumerate() {
            let DefInfo::Adt(a) = info else { continue };
            let Some(&m) = a.vtable.get(slot as usize) else {
                continue;
            };
            let mut cur = Some(DefId(i as u32));
            let mut depth = 0;
            while let Some(c) = cur {
                if c == class {
                    self.facts.edges.push(m);
                    break;
                }
                depth += 1;
                cur = match self.cx.adt(c).and_then(|a| a.base) {
                    Some(b) if depth < 64 => self.cx.class_of(b).map(|(d, _)| d),
                    _ => None,
                };
            }
        }
    }
}

impl crate::visit::VisitMut for FactsOf<'_, '_> {
    fn expr(&mut self, e: &mut hir::Expr) {
        match &e.kind {
            E::Global(d) => {
                if self.facts.direct.is_none() {
                    if let Some(g) = self.cx.global(*d).filter(|g| g.stored) {
                        self.facts.direct = Some(format!("uses `{}`", g.name));
                    }
                }
            }
            E::Closure(c) | E::FnRef(c, _) => self.facts.edges.push(*c),
            E::New { def, .. } => {
                let ctor = self.cx.adt(*def).and_then(|a| a.ctor);
                self.facts.edges.extend(ctor);
            }
            E::Call { callee, args } => match callee {
                hir::Callee::Def(f, _) => self.facts.edges.push(*f),
                hir::Callee::Indirect(_) => self.unknown(),
                hir::Callee::Virtual { slot } => {
                    match args.first().and_then(|a| self.cx.class_of(a.ty)) {
                        Some((c, _)) => self.virtual_methods(c, *slot),
                        None => self.unknown(),
                    }
                }
                hir::Callee::Dyn { slot } => {
                    let recv = args.first().map(|a| self.cx.ty.kind(a.ty).clone());
                    match recv {
                        Some(TyKind::Dyn(i, _)) => self.iface_methods(i, *slot),
                        _ => self.unknown(),
                    }
                }
                hir::Callee::ParamMethod { iface, slot, .. } => self.iface_methods(*iface, *slot),
                hir::Callee::Intrinsic(hir::Intrinsic::MutexWith) => {
                    if let [_, cb] = args.as_slice() {
                        self.withs.push((cb.clone(), cb.span));
                    }
                }
                hir::Callee::Intrinsic(_) => {}
            },
            _ => {}
        }
    }
}
