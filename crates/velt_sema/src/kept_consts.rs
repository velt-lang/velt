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
/// a module-level `let`, and each such use refers to that one object (a move shares it). A
/// `static readonly` field is kept the same way, as Node evaluates it once with its class. Not
/// for a constant of the standard library, `import.meta` (whose fields are read-only), one whose type is an error, or one holding a promise (`driver::promise_in_state`):
/// those keep their errors. Returns whether `d` is module state.
pub(crate) fn keep(cx: &mut Ctx, d: DefId) -> bool {
    let Some(g) = cx.global(d) else { return false };
    if g.stored {
        return true;
    }
    let keepable = g.state == crate::defs::BodyState::Done
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

/// Is global `d` module state that the program may change: a module-level `let`, a constant
/// whose value is not a constant (`const seen = new Map()`), or one [`keep`] or
/// [`keep_readers`] kept? Each is one object the program can modify.
pub(crate) fn changes(cx: &Ctx, d: DefId) -> bool {
    cx.global(d).is_some_and(|g| g.stored)
}

/// Once every use is checked: a module constant or `static readonly` field whose initializer
/// reads module state that changes (a module-level `let`, a constant kept there), directly or through the functions it calls,
/// would see those changes if it were evaluated at each use, where Node evaluated it once before
/// them. It is kept in the module state too (evaluated once, in Node's order), and counts as
/// changing for the constants that read it in turn (`const C = getK()` may be `K` itself), until
/// nothing changes (a walk over who reads and calls what, so each is handled once). One that
/// can't be kept (it holds a promise) is an error.
pub(crate) fn keep_readers(cx: &mut Ctx) {
    let consts: Vec<DefId> = (0..cx.info.len())
        .map(|i| DefId(i as u32))
        .filter(|&d| {
            cx.global(d).is_some_and(|g| {
                g.init.is_some()
                    && !g.mutable
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
    let fns = reachable_fns(cx, &inits);
    // Who reads each global, and who calls or creates each function: a constant or a function
    // that reads changing state makes its readers and callers read it too.
    let mut readers: FxHashMap<DefId, Vec<DefId>> = FxHashMap::default();
    let mut callers: FxHashMap<DefId, Vec<DefId>> = FxHashMap::default();
    for (&n, facts) in inits.iter().chain(fns.iter()) {
        for &g in &facts.globals {
            readers.entry(g).or_default().push(n);
        }
        for &f in &facts.fns {
            callers.entry(f).or_default().push(n);
        }
    }
    // The constants and functions that read changing state, each handled once.
    let mut reaching: FxHashSet<DefId> = FxHashSet::default();
    let mut work: Vec<DefId> = (readers.iter())
        .filter(|(&g, _)| changes(cx, g))
        .flat_map(|(_, rs)| rs.iter().copied())
        .collect();
    while let Some(n) = work.pop() {
        if !reaching.insert(n) {
            continue;
        }
        if let Some(cs) = callers.get(&n) {
            work.extend(cs.iter().copied());
        }
        if !inits.contains_key(&n) || changes(cx, n) {
            continue;
        }
        // A constant that reads changing state: kept, it changes for its readers in turn.
        if keep(cx, n) {
            if let Some(rs) = readers.get(&n) {
                work.extend(rs.iter().copied());
            }
        } else if let Some(g) = cx.global(n) {
            let (ty, span) = (g.ty, g.src.span);
            crate::body::promise_in_state(cx, n, ty, span);
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

/// Every function reachable from the initializers `inits`, through the functions they call or
/// create, with its facts (each collected once).
fn reachable_fns(cx: &Ctx, inits: &FxHashMap<DefId, Facts>) -> FxHashMap<DefId, Facts> {
    let mut fns: FxHashMap<DefId, Facts> = FxHashMap::default();
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
    fns
}
