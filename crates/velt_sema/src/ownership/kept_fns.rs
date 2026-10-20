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
//!   ([`KeptFns::forced`]), and its callers hand over (or share) an escaping closure. A method
//!   with fixed modes reached directly (an override through its base class) counts as such a
//!   callee too.
//!
//! A type parameter may stand for a function type: a closure literal passed to a parameter of
//! a generic type always escapes (`super::patch`), a function variable passed to one counts as
//! passed to a callee that may keep it, and a function with fixed modes keeping a borrowed
//! variable of a generic type (`put(t: T) { this.items.push(t); }`) turns the rules on. So a
//! generic variable never holds a closure living in a frame.
//!
//! An escaping closure passed as a borrowed argument may still get a frame environment when it
//! owns nothing; one holding shared cells (a captured variable assigned later) gets a heap
//! environment (velt_vir closure.rs), since sharing an env with a drop entry retains it.
//!
//! The only closure values that live in a frame and capture by reference are then literals
//! passed to borrowed parameters of functions with inferred modes, held in parameters that are
//! never kept and never passed to a callee with fixed modes (and `const` closures that are only
//! called, `super::local_closures`, which are never passed on at all). So a parameter with
//! fixed modes only ever holds a closure with an environment of its own (or a copy of a frame
//! environment of value captures, which its clone function copies to the heap), and keeping
//! it is a share (`super::validate`). A captured variable is kept the same way when it comes
//! from a local of the creating function: a local owns its closure.

use rustc_hash::{FxHashMap, FxHashSet};

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
    pub forced: FxHashSet<(DefId, LocalId)>,
    /// The function creating each closure.
    pub parents: FxHashMap<DefId, DefId>,
}

/// Record the function creating each closure of `fns`.
pub(super) fn find_parents(cx: &mut Ctx, fns: &[DefId]) {
    let mut parents = FxHashMap::default();
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
    borrowed: &FxHashSet<LocalId>,
    to_fixed: bool,
) -> FxHashSet<LocalId> {
    let mut out = FxHashSet::default();
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
        E::Call { callee, args } if to_fixed => {
            // Passed to a callee that may keep it, or to a parameter of a generic type (which
            // may hand it to one).
            let all = may_keep_args(cx, callee, args);
            let generic = generic_param_args(cx, callee, args);
            for (k, a) in args.iter().enumerate() {
                let kept = all || generic.get(k).copied().unwrap_or(false);
                if let Some(l) = local_of(a).filter(|l| kept && borrowed.contains(l)) {
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
        // A variable of a generic type may hold a function too (`put(t: T) { this.items.push(t);
        // }` in a class implementing `Sink<T>`).
        let borrowed: FxHashSet<LocalId> = super::fn_values::borrowed_fn_locals(cx, d, &f)
            .into_iter()
            .chain(borrowed_generic_locals(cx, d, &f))
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

/// The borrowed parameters and captures of `f` (body of `d`) whose type mentions a type
/// parameter: instantiated with a function type, they may hold a closure in a caller's frame
/// (`forward<T>(reg: (t: T) => void, t: T) { reg(t); }`).
fn borrowed_generic_locals(cx: &Ctx, d: DefId, f: &FnDef) -> FxHashSet<LocalId> {
    let kinds = &cx.fn_info(d).local_kinds;
    let borrowed = |m: PassMode| matches!(m, PassMode::Borrow | PassMode::BorrowMut);
    let params = f
        .params
        .iter()
        .filter(|p| kinds.get(p.local.0 as usize) == Some(&LocalKind::Param) && borrowed(p.mode))
        .map(|p| p.local);
    let caps = f
        .captures
        .iter()
        .filter(|c| borrowed(c.mode))
        .map(|c| c.inner);
    params
        .chain(caps)
        .filter(|l| cx.has_type_params(f.body.locals[l.0 as usize].ty))
        .collect()
}

/// May the callee of a call keep its arguments? A function value or interface method can
/// (`super::patch::fixed_callee`), and with [`KeptFns::on`] so can any method with fixed modes
/// (an override reached through `super` or a class type).
pub(super) fn may_keep_args(cx: &Ctx, callee: &Callee, args: &[Expr]) -> bool {
    if super::patch::fixed_callee(cx, callee, args) {
        return true;
    }
    cx.kept_fns.on
        && matches!(
            super::patch::call_target(cx, callee, args),
            Some(super::patch::Target::Fn(m)) if cx.fn_info(m).fixed_modes
        )
}

/// Per argument of a direct call: is the parameter it is passed to declared with a type that
/// mentions a type parameter (other than a function type)? A value passed there may be kept
/// as that type, whatever the callee's inferred modes say about its own use.
pub(super) fn generic_param_args(cx: &Ctx, callee: &Callee, args: &[Expr]) -> Vec<bool> {
    let Some(super::patch::Target::Fn(d)) = super::patch::call_target(cx, callee, args) else {
        return vec![];
    };
    let info = cx.fn_info(d);
    info.this
        .iter()
        .map(|_| false)
        .chain(
            info.params
                .iter()
                .map(|p| cx.has_type_params(p.ty) && !super::fn_values::is_fn(cx, p.ty)),
        )
        .collect()
}
