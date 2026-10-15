//! Module state (#76): module-level `let`s become slots of one per-task tuple
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
