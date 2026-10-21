//! Module constants kept in the module state because of how the program uses them (#1383). A
//! module constant that is only read is evaluated at each use (VIR `global()`); one that a use
//! takes over, modifies or compares by identity would show that each use is a new value, where
//! Node evaluates it once. [`keep`] makes it module state at that use, like a module-level
//! `let`. Once every use is checked, [`keep_readers`] keeps the constants whose initializers
//! read module state that changes: evaluated at each use, they would see those changes.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{self, DefId, ExprKind as E};

/// Keep module constant `d` in the module state: a use takes its value over (binds, stores or
/// returns it), modifies it or compares it by identity, where a value evaluated at each use
/// would be a different object from the one Node evaluates once. Kept, it is evaluated once like
/// a module-level `let`, and each such use refers to that one object (a move shares it). Not for
/// a constant of the standard library, a `static readonly` field, `import.meta` (whose fields are
/// read-only), one whose type is an error, or one holding a promise (`driver::promise_in_state`):
/// those keep their errors. Returns whether `d` is module state.
pub(crate) fn keep(cx: &mut Ctx, d: DefId) -> bool {
    let Some(g) = cx.global(d) else { return false };
    if g.stored {
        return true;
    }
    let keepable = g.state == crate::defs::BodyState::Done
        && g.src.owner.is_none()
        && g.name != velt_syntax::ast::IMPORT_META
        && !cx.modules[g.module].is_std
        && g.ty != cx.ty.error
        && !crate::body::holds_promise_by_value(cx, g.ty, 0);
    if keepable {
        set_stored(cx, d, true);
        cx.kept_consts.push(d);
    }
    keepable
}

/// Undo [`keep`] for `kept` (the constants a rolled-back check kept).
pub(crate) fn unkeep(cx: &mut Ctx, kept: Vec<DefId>) {
    for d in kept {
        set_stored(cx, d, false);
    }
}

fn set_stored(cx: &mut Ctx, d: DefId, stored: bool) {
    if let DefInfo::Global(g) = &mut cx.info[d.0 as usize] {
        g.stored = stored;
    }
}

/// Is global `d` module state that the program may change: a module-level `let`, or a constant
/// [`keep`] or [`keep_readers`] kept?
pub(crate) fn changes(cx: &Ctx, d: DefId) -> bool {
    cx.global(d).is_some_and(|g| g.mutable) || cx.kept_consts.contains(&d)
}

/// Once every use is checked: a top-level constant whose initializer reads module state that
/// changes (a module-level `let`, a kept constant), directly or through the functions it calls,
/// would see those changes if it were evaluated at each use, where Node evaluated it once before
/// them. It is kept in the module state too (evaluated once, in Node's order), and counts as
/// changing for the constants that read it in turn (`const C = getK()` may be `K` itself), until
/// nothing changes. One that can't be kept (it holds a promise) is an error.
pub(crate) fn keep_readers(cx: &mut Ctx) {
    let consts: Vec<DefId> = (0..cx.info.len())
        .map(|i| DefId(i as u32))
        .filter(|&d| {
            cx.global(d).is_some_and(|g| {
                g.init.is_some()
                    && !g.mutable
                    && g.src.owner.is_none()
                    && g.ty != cx.ty.error
                    && !cx.modules[g.module].is_std
            })
        })
        .collect();
    // A constant first checked inside a check that was rolled back keeps what that check's
    // `keep` did: an initializer that takes another constant over (`const B = { a: A }`).
    let mut inits = FxHashMap::default();
    for &d in &consts {
        let Some(init) = cx.global(d).and_then(|g| g.init.clone()) else {
            continue;
        };
        let facts = Facts::of_expr(init);
        for &a in &facts.shared {
            keep(cx, a);
        }
        inits.insert(d, facts);
    }
    let mut fns: FxHashMap<DefId, Facts> = FxHashMap::default();
    let mut refused = FxHashSet::default();
    loop {
        let reaching = reaching_fns(cx, &inits, &mut fns);
        let mut changed = false;
        for &d in &consts {
            if cx.kept_consts.contains(&d) || refused.contains(&d) {
                continue;
            }
            let facts = &inits[&d];
            let reads = facts.globals.iter().any(|&g| changes(cx, g))
                || facts.fns.iter().any(|f| reaching.contains(f));
            if !reads {
                continue;
            }
            if keep(cx, d) {
                if !cx.kept_consts.contains(&d) {
                    cx.kept_consts.push(d);
                }
                changed = true;
            } else if let Some(g) = cx.global(d) {
                refused.insert(d);
                let (ty, span) = (g.ty, g.src.span);
                crate::body::promise_in_state(cx, d, ty, span);
            }
        }
        if !changed {
            return;
        }
    }
}

/// What evaluating an expression or a function body may reach: the globals it reads, the
/// functions it calls or creates, and the constants it takes over (`Share` of a constant, as
/// `global_read` makes them).
#[derive(Default)]
struct Facts {
    globals: Vec<DefId>,
    fns: Vec<DefId>,
    shared: Vec<DefId>,
}

impl Facts {
    fn of_expr(mut e: hir::Expr) -> Facts {
        let mut facts = Facts::default();
        crate::visit::expr(&mut e, &mut facts);
        facts
    }
}

impl crate::visit::VisitMut for Facts {
    fn expr(&mut self, e: &mut hir::Expr) {
        match &e.kind {
            E::Global(d) => self.globals.push(*d),
            E::Closure(c) | E::FnRef(c, _) => self.fns.push(*c),
            E::Call {
                callee: hir::Callee::Def(g, _),
                ..
            } => self.fns.push(*g),
            _ => {}
        }
        if let E::Call {
            callee: hir::Callee::Intrinsic(hir::Intrinsic::Share),
            args,
        } = &e.kind
        {
            if let Some(E::Global(d)) = args.first().map(|a| &a.kind) {
                self.shared.push(*d);
            }
        }
    }
}

/// The functions reachable from the initializers `inits` that read changing module state,
/// directly or through the functions they call or create. Each function's facts are collected
/// once (`fns` carries them between rounds).
fn reaching_fns(
    cx: &Ctx,
    inits: &FxHashMap<DefId, Facts>,
    fns: &mut FxHashMap<DefId, Facts>,
) -> FxHashSet<DefId> {
    // Every function reachable from an initializer, with its facts.
    let mut work: Vec<DefId> = inits.values().flat_map(|f| f.fns.iter().copied()).collect();
    while let Some(f) = work.pop() {
        if fns.contains_key(&f) {
            continue;
        }
        let facts = match cx.defs[f.0 as usize].as_ref() {
            Some(hir::Def::Fn(def)) => {
                let mut facts = Facts::default();
                crate::visit::block(&mut def.body.block.clone(), &mut facts);
                facts
            }
            _ => Facts::default(),
        };
        work.extend(facts.fns.iter().copied());
        fns.insert(f, facts);
    }
    // Those that read changing state, then their callers, until none is added.
    let mut reaching: FxHashSet<DefId> = (fns.iter())
        .filter(|(_, facts)| facts.globals.iter().any(|&g| changes(cx, g)))
        .map(|(&f, _)| f)
        .collect();
    loop {
        let more: Vec<DefId> = (fns.iter())
            .filter(|(f, facts)| {
                !reaching.contains(*f) && facts.fns.iter().any(|g| reaching.contains(g))
            })
            .map(|(&f, _)| f)
            .collect();
        if more.is_empty() {
            return reaching;
        }
        reaching.extend(more);
    }
}
