//! After the inference fixpoint:
//! - named functions used as function values may be called with aliasing arguments (calls
//!   through a function value don't check exclusivity between their arguments), so their
//!   modified params lose `BorrowMut`'s no-alias guarantee: they become `Borrow` with
//!   `LocalDef::mutable` set (like closure params; direct call sites stay checked);
//! - reassigning a non-Copy param of a function with a fixed ABI (closures, overridden and
//!   interface methods) is an error: it cannot own its argument;
//! - arguments for "soft" owned params (owned only because the callee reassigns them) are
//!   recorded as soft moves: a caller that uses the value again passes a clone;
//! - changing a `for…of` element of a value read out of a `shared` (or of what a method of one
//!   returns), or a variable bound to a part of one, is an error: the binding holds a copy,
//!   where JavaScript changes the shared value.

use rustc_hash::FxHashMap;
use velt_common::Diagnostic;

use crate::body::places::is_place;
use crate::ctx::Ctx;
use crate::defs::FnKind;
use crate::hir::{Callee, DefId, Expr, ExprKind as E, FnDef, LocalId, PassMode};
use crate::visit;

use super::evidence;

/// Demote the modified params of named functions used as values (module docs).
pub(super) fn demote_fn_values(cx: &mut Ctx) {
    let values: Vec<DefId> = cx.fn_values.iter().map(|(d, ..)| *d).collect();
    for d in values {
        let info = cx.fn_info_mut(d);
        let mut demoted = vec![];
        for (k, p) in info.params.iter_mut().enumerate() {
            if p.mode == PassMode::BorrowMut {
                p.mode = PassMode::Borrow;
                demoted.push(k);
            }
        }
        let skip = usize::from(info.this.is_some());
        if let Some(crate::hir::Def::Fn(f)) = &mut cx.defs[d.0 as usize] {
            for k in demoted {
                if let Some(p) = f.params.get(skip + k) {
                    f.body.locals[p.local.0 as usize].mutable = true;
                }
            }
        }
    }
}

/// Fixed-ABI functions cannot reassign a borrowed (non-Copy) param.
pub(super) fn reassigned_fixed_params(cx: &mut Ctx, d: DefId, f: &mut FnDef) {
    let info = cx.fn_info(d);
    if !info.fixed_modes || info.is_async || info.is_generator || info.kind == FnKind::Extern {
        return;
    }
    let what = match info.kind {
        FnKind::Closure => "a closure",
        _ => "an overridden or interface method",
    };
    let ev = evidence::collect(cx, &mut f.body.block, f.captures.len()..f.params.len());
    let first = f.captures.len() + usize::from(f.self_ty.is_some());
    for p in &f.params[first..] {
        let Some(&at) = ev.reassigned.get(&p.local) else {
            continue;
        };
        if matches!(p.mode, PassMode::Copy | PassMode::Owned) {
            continue;
        }
        let name = &f.body.locals[p.local.0 as usize].name;
        cx.error(
            Diagnostic::error(
                format!("cannot assign to parameter `{name}` of {what}"),
                at,
            )
            .with_note(format!(
                "its argument stays the caller's; assign a local copy instead: `let local = {name}.clone();`"
            )),
        );
    }
}

/// Record the arguments of soft owned params in `d`'s soft moves.
pub(super) fn soft_args(cx: &mut Ctx, d: DefId, f: &mut FnDef) {
    let mut spans = vec![];
    visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
        let E::Call {
            callee: Callee::Def(callee, _),
            args,
        } = &e.kind
        else {
            return;
        };
        let info = cx.fn_info(*callee);
        let skip = usize::from(info.this.is_some());
        for &k in &info.soft_params {
            let Some(mut a) = args.get(skip + k) else {
                continue;
            };
            while let E::WrapSome(x) | E::Upcast(x) | E::Downcast(x) = &a.kind {
                a = x;
            }
            if is_place(a) && super::soft::is_moved_place(a) {
                spans.push((a.span, a.ty));
            }
        }
    });
    for (span, ty) in spans {
        if cx.is_shared_value(ty) {
            cx.fn_info_mut(d).soft_moves.push(span);
        }
    }
}

/// Bindings of a value read out of a `shared` that the body changes (module docs): `for…of`
/// elements (`Ctx::frozen_elems`), variables bound to a part (`const ps = s.ports`), and,
/// through them, elements of loops over those (`Ctx::loop_roots`) and variables bound to them.
pub(super) fn frozen_bindings(cx: &mut Ctx, f: &mut FnDef) {
    if !cx.frozen_reads {
        return;
    }
    let edges = super::derived::edges(cx, f);
    let elem = |l: &crate::hir::LocalDef| {
        cx.frozen_elems.contains(&l.span) || cx.loop_roots.contains_key(&l.span)
    };
    let mut binds: FxHashMap<LocalId, bool> = FxHashMap::default();
    // Locals only holding a part (`o.f = s.items[0]`, `[s.items[0]]`): only a change through
    // them changes the part.
    let mut held: rustc_hash::FxHashSet<LocalId> = rustc_hash::FxHashSet::default();
    for (i, l) in f.body.locals.iter().enumerate() {
        if cx.frozen_elems.contains(&l.span) {
            binds.insert(LocalId(i as u32), true);
        }
    }
    // Bounded: each round adds a local, or makes a held one a part.
    for _ in 0..=2 * f.body.locals.len() + 1 {
        let mut grew = false;
        for e in &edges {
            let from = match e.src {
                None => Some(false),
                Some(x) => binds.contains_key(&x).then(|| held.contains(&x)),
            };
            let Some(from_held) = from else { continue };
            let as_held = e.held || from_held && !e.projected;
            match binds.contains_key(&e.bound) {
                false => {
                    binds.insert(e.bound, elem(&f.body.locals[e.bound.0 as usize]));
                    if as_held {
                        held.insert(e.bound);
                    }
                    grew = true;
                }
                true if !as_held && held.remove(&e.bound) => grew = true,
                true => {}
            }
        }
        if !grew {
            break;
        }
    }
    let ev = evidence::collect(cx, &mut f.body.block, f.captures.len()..f.params.len());
    let mut errors = direct_writes(cx, f);
    let written: Vec<velt_common::Span> = errors
        .iter()
        .filter_map(|d| d.labels.first().map(|l| l.span))
        .collect();
    let mut binds: Vec<(LocalId, bool)> = binds.into_iter().collect();
    binds.sort();
    let changed = |l: &LocalId| match held.contains(l) {
        true => ev.deep.contains(l),
        false => ev.mutated.contains(l) || ev.modified.contains(l) || ev.weak.contains(l),
    };
    // A hidden local of a named one reported (`(o.it ?? x).v = 1`) is that write again.
    let named: rustc_hash::FxHashSet<LocalId> = (binds.iter())
        .map(|&(l, _)| l)
        .filter(|l| changed(l) && !f.body.locals[l.0 as usize].name.starts_with('<'))
        .collect();
    for (l, elem) in binds {
        let local = &f.body.locals[l.0 as usize];
        let changed = changed(&l);
        let of_named = local.name.starts_with('<')
            && (edges.iter()).any(|e| e.bound == l && e.src.is_some_and(|x| named.contains(&x)));
        // A map loop's hidden local holds the source; the body can't name it. One of `x!` or
        // `?.` inside a write reported as a write is that write.
        let covered = local.name.starts_with('<')
            && written.iter().any(|w| {
                w.file == local.span.file && w.lo <= local.span.lo && local.span.hi <= w.hi
            });
        if local.name.starts_with("<map@")
            || covered
            || of_named
            || !changed
            || cx.is_copy(local.ty)
        {
            continue;
        }
        let (name, tn, span) = (local.name.clone(), cx.display(local.ty), local.span);
        let what = match elem {
            true => "the loop changes",
            false => "this changes",
        };
        // A hidden local (`<nullish@N>` of `x!`, `<chain@N>` of `?.`) has no name to give.
        let name = match name.starts_with('<') {
            true => String::new(),
            false => format!("`{name}`, "),
        };
        errors.push(
            Diagnostic::error(
                format!("{what} {name}a `{tn}` read out of a `shared` value"),
                span,
            )
            .with_note(
                "a shared value is read-only, since other tasks read it at the same time: \
                 change a copy (`.clone()`, `.slice()`), or share a `Mutex`",
            ),
        );
    }
    report(cx, errors);
}

/// Writes through what a method or getter of a value read out of a `shared` returns, in the
/// function reading it (`s.m.get(k)!.v = 1`, `s.head.tags.push(x)`): no local is to blame, so
/// `frozen_bindings` would not see them.
fn direct_writes(cx: &mut Ctx, f: &mut FnDef) -> Vec<Diagnostic> {
    let mut found = vec![];
    let closures = evidence::closure_locals(cx, &mut f.body.block);
    visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
        // A callback changing what it is given, passed with a part (`[s.items[0]].forEach(…)`).
        if let E::Call {
            callee: Callee::Def(d, _),
            args,
        } = &e.kind
        {
            if evidence::changing_callback(cx, &closures, *d, args) {
                for a in (args.iter()).filter(|a| evidence::callback(cx, &closures, a).is_none()) {
                    let r = super::derived::roots(cx, a);
                    if (r.frozen || r.held_frozen) && through_call(a) {
                        found.push((a.span, a.ty));
                    }
                }
            }
        }
        let place = match &e.kind {
            E::Assign { place, .. } | E::CompoundAssign { place, .. } => &**place,
            E::Field {
                mode: crate::hir::UseMode::BorrowMut,
                ..
            }
            | E::Index {
                mode: crate::hir::UseMode::BorrowMut,
                ..
            } => &*e,
            _ => return,
        };
        let fresh_slot = matches!(&place.kind, E::Field { base, .. } | E::Index { base, .. } if super::derived::fresh(cx, base));
        if !matches!(place.kind, E::Local(..)) && !fresh_slot && through_call(place) {
            let r = super::derived::roots(cx, place);
            if r.frozen {
                found.push((place.span, place.ty));
            }
        }
    });
    found.sort_by_key(|(s, _)| (s.lo, s.hi));
    found.dedup_by_key(|(s, _)| (s.lo, s.hi));
    let mut out = vec![];
    for (span, ty) in found {
        let tn = cx.display(ty);
        out.push(
            Diagnostic::error(
                format!("this changes a `{tn}`, a part of a value read out of a `shared`"),
                span,
            )
            .with_note(
                "a shared value is read-only, since other tasks read it at the same time: change \
                 a copy (`.clone()`), or share a `Mutex`",
            ),
        );
    }
    out
}

/// Report `errors`, one per place: a write through a hidden local of `x!` is found both as a
/// write and as a change of the local.
fn report(cx: &mut Ctx, mut errors: Vec<Diagnostic>) {
    let at = |d: &Diagnostic| d.labels.first().map(|l| (l.span.file, l.span.lo));
    errors.sort_by_key(|d| at(d));
    errors.dedup_by_key(|d| at(d));
    for d in errors {
        cx.error(d);
    }
}

/// Does place `e` go through a call (a method's or getter's result) on the way to its root?
/// Places rooted directly in a read are checked where they are written (`require_mutable`).
fn through_call(e: &Expr) -> bool {
    match &e.kind {
        E::Call {
            callee: Callee::Intrinsic(crate::hir::Intrinsic::SharedRead),
            ..
        } => false,
        E::Call { .. }
        | E::Match { .. }
        | E::If { .. }
        | E::Logical { .. }
        | E::Block(_)
        | E::ArrayLit(_)
        | E::Tuple(_)
        | E::AdtLit { .. }
        | E::New { .. } => true,
        E::Field { base, .. }
        | E::Index { base, .. }
        | E::UnwrapSome(base, _)
        | E::UnwrapVariant { expr: base, .. }
        | E::Downcast(base)
        | E::Upcast(base) => through_call(base),
        _ => false,
    }
}
