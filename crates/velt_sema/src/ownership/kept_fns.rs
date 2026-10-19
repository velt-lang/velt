//! Function values kept by functions with fixed parameter modes (closures, overridden and
//! interface methods): `onCleanup((fn) => cleanupFn = fn)`, `subscribe(handler) {
//! subscribers.push(handler); }`. Their function-typed parameters are borrowed, and a closure
//! literal passed directly to a borrowed parameter may capture by reference and live in its
//! caller's frame (docs/reference/functions.md "Captures"), so keeping a copy of one is an
//! error (`super::fn_values`) unless every value that can reach the parameter is a closure
//! with an environment of its own.
//!
//! When a program keeps such a parameter (or a captured function variable that reaches one),
//! [`KeptFns::on`] is set and ownership inference runs again under two extra rules:
//! - every closure literal passed to a callee with fixed modes escapes, including one that
//!   forwards a borrowed function (`super::patch`), so it captures by value and owns a heap
//!   environment;
//! - a borrowed function variable that a body keeps, or passes to a callee with fixed modes, is
//!   traced through the captures to the variable it comes from ([`root`]); when that is a
//!   borrowed parameter of a function whose modes are inferred, the parameter becomes owned
//!   ([`KeptFns::forced`]), and its callers hand over (or share) an escaping closure.
//!
//! The only closure values that live in a frame and capture by reference are then literals
//! passed to borrowed parameters of functions with inferred modes, held in parameters that are
//! never kept and never passed to a callee with fixed modes (and `const` closures that are only
//! called, `super::local_closures`, which are never passed on at all). So a parameter with
//! fixed modes only ever holds a closure with an environment of its own (or a copy of a frame
//! environment of value captures, which its clone function copies to the heap), and keeping
//! it is a share (`super::validate`). A captured variable is kept the same way when it comes
//! from a local of the creating function: a local owns its closure.

use std::collections::{HashMap, HashSet};

use crate::body::LocalKind;
use crate::ctx::Ctx;
use crate::hir::{
    Callee, Def, DefId, Expr, ExprKind as E, FnDef, Intrinsic, LocalId, PassMode, UseMode,
};
use crate::visit;

/// Program-wide state of the rules above.
#[derive(Default)]
pub(crate) struct KeptFns {
    /// Some function with fixed modes keeps a borrowed function value.
    pub on: bool,
    /// Borrowed parameters of functions with inferred modes that must own their closure.
    pub forced: HashSet<(DefId, LocalId)>,
    /// The function creating each closure.
    pub parents: HashMap<DefId, DefId>,
}

/// Record the function creating each closure of `fns`.
pub(super) fn find_parents(cx: &mut Ctx, fns: &[DefId]) {
    let mut parents = HashMap::new();
    for &d in fns {
        let Some(Def::Fn(f)) = &mut cx.defs[d.0 as usize] else {
            continue;
        };
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            if let E::Closure(c) = e.kind {
                parents.insert(c, d);
            }
        });
    }
    cx.kept_fns.parents = parents;
}

/// Where a variable of `f` (body of `d`) comes from: `Some((def, param))` for a parameter
/// (through any captures), `None` for a local variable of the function that created it.
pub(super) fn root(cx: &Ctx, d: DefId, f: &FnDef, l: LocalId) -> Option<(DefId, LocalId)> {
    let (mut d, mut l) = (d, l);
    let mut caps = f.captures.clone();
    for _ in 0..64 {
        match cx.fn_info(d).local_kinds.get(l.0 as usize) {
            Some(LocalKind::Param) => return Some((d, l)),
            Some(LocalKind::Capture) => {
                let outer = caps.iter().find(|c| c.inner == l)?.outer;
                d = *cx.kept_fns.parents.get(&d)?;
                l = outer;
                caps = match &cx.defs[d.0 as usize] {
                    Some(Def::Fn(p)) => p.captures.clone(),
                    // The creating function is being visited: it has no captures to follow
                    // that its own body's check would not see.
                    _ => vec![],
                };
            }
            Some(LocalKind::Let | LocalKind::Const | LocalKind::Using | LocalKind::Temp) => {
                return None
            }
            // `this`, a pattern or `for...of` binding: treated as a parameter of its own
            // (never safe to keep through, see `sharable`).
            _ => return Some((d, l)),
        }
    }
    Some((d, l))
}

/// May `f` (body of `d`) keep a share of its borrowed function variable `l`? Yes when `l`
/// comes from a local of the function that created it, or (with [`KeptFns::on`]) from a
/// parameter with fixed modes or an owned parameter.
pub(super) fn sharable(cx: &Ctx, d: DefId, f: &FnDef, l: LocalId) -> bool {
    let Some((rd, rl)) = root(cx, d, f, l) else {
        return true;
    };
    if !cx.kept_fns.on || cx.fn_info(rd).local_kinds.get(rl.0 as usize) != Some(&LocalKind::Param) {
        return false;
    }
    if cx.fn_info(rd).fixed_modes || cx.kept_fns.forced.contains(&(rd, rl)) {
        return true;
    }
    let params = match (rd == d, &cx.defs[rd.0 as usize]) {
        (true, _) => &f.params,
        (false, Some(Def::Fn(p))) => &p.params,
        _ => return false,
    };
    params
        .iter()
        .any(|p| p.local == rl && p.mode == PassMode::Owned)
}

/// The borrowed function variables of `f` (`borrowed`) that the body keeps (moves, copies or
/// captures by value in an escaping closure), and with `to_fixed` also those it passes to a
/// callee with fixed modes.
pub(super) fn exposed(
    cx: &Ctx,
    f: &mut FnDef,
    borrowed: &HashSet<LocalId>,
    to_fixed: bool,
) -> HashSet<LocalId> {
    let mut out = HashSet::new();
    visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| match &e.kind {
        E::Local(l, UseMode::Move) if borrowed.contains(l) => {
            out.insert(*l);
        }
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::Clone | Intrinsic::Share),
            args,
        } => {
            if let [Expr {
                kind: E::Local(l, _),
                ..
            }] = args.as_slice()
            {
                if borrowed.contains(l) {
                    out.insert(*l);
                }
            }
        }
        E::Call { callee, args } if to_fixed && super::patch::fixed_callee(cx, callee, args) => {
            for a in args {
                if let Some(l) = local_of(a).filter(|l| borrowed.contains(l)) {
                    out.insert(l);
                }
            }
        }
        E::Closure(c) if cx.fn_info(*c).escaping => {
            if let Some(Def::Fn(cf)) = &cx.defs[c.0 as usize] {
                for cap in cf.captures.iter().filter(|c| c.mode == PassMode::Owned) {
                    if borrowed.contains(&cap.outer) {
                        out.insert(cap.outer);
                    }
                }
            }
        }
        _ => {}
    });
    out
}

/// The variable an argument passes (through upcasts and `Some` wrapping).
fn local_of(e: &Expr) -> Option<LocalId> {
    match &e.kind {
        E::Local(l, _) => Some(*l),
        E::Upcast(inner) | E::WrapSome(inner) => local_of(inner),
        _ => None,
    }
}

/// Does some function with fixed modes keep a borrowed function variable (the errors
/// `super::validate` would report otherwise)?
pub(super) fn any_kept(cx: &mut Ctx, fns: &[DefId]) -> bool {
    for &d in fns {
        let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        // A parameter of a function with inferred modes that the body keeps is owned (the HIR
        // params get their inferred modes only after inference).
        let fixed = cx.fn_info(d).fixed_modes;
        let kinds = &cx.fn_info(d).local_kinds;
        let borrowed: HashSet<LocalId> = super::fn_values::borrowed_fn_locals(cx, d, &f)
            .into_iter()
            .filter(|l| fixed || kinds.get(l.0 as usize) != Some(&LocalKind::Param))
            .collect();
        let kept = !borrowed.is_empty() && !exposed(cx, &mut f, &borrowed, false).is_empty();
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
        if kept {
            return true;
        }
    }
    false
}

/// With [`KeptFns::on`]: the parameters of functions with inferred modes that `f` (body of
/// `d`) keeps or passes to a callee with fixed modes, newly forced to be owned.
pub(super) fn force_roots(cx: &mut Ctx, d: DefId, f: &mut FnDef) -> Vec<DefId> {
    if !cx.kept_fns.on {
        return vec![];
    }
    let borrowed = super::fn_values::borrowed_fn_locals(cx, d, f);
    if borrowed.is_empty() {
        return vec![];
    }
    let mut changed = vec![];
    for l in exposed(cx, f, &borrowed, true) {
        let Some((rd, rl)) = root(cx, d, f, l) else {
            continue;
        };
        let param = cx.fn_info(rd).local_kinds.get(rl.0 as usize) == Some(&LocalKind::Param);
        if param && !cx.fn_info(rd).fixed_modes && cx.kept_fns.forced.insert((rd, rl)) {
            changed.push(rd);
        }
    }
    changed
}
