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
    let ev = evidence::collect(cx, &mut f.body.block);
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
    for (i, l) in f.body.locals.iter().enumerate() {
        if cx.frozen_elems.contains(&l.span) {
            binds.insert(LocalId(i as u32), true);
        }
    }
    // A local holding a part (`obj.f = s.items[0]`) counts only for changes through it.
    let mut held: rustc_hash::FxHashSet<LocalId> = rustc_hash::FxHashSet::default();
    for (l, r) in &edges {
        if r.frozen && !binds.contains_key(l) {
            binds.insert(*l, elem(&f.body.locals[l.0 as usize]));
            if r.held {
                held.insert(*l);
            }
        }
    }
    // Bounded: each round adds a local.
    for _ in 0..=f.body.locals.len() {
        let before = binds.len();
        for (l, r) in &edges {
            if !binds.contains_key(l) && r.locals.iter().any(|x| binds.contains_key(x)) {
                binds.insert(*l, elem(&f.body.locals[l.0 as usize]));
                if r.held {
                    held.insert(*l);
                }
            }
        }
        if binds.len() == before {
            break;
        }
    }
    if binds.is_empty() {
        return;
    }
    let ev = evidence::collect(cx, &mut f.body.block);
    let mut binds: Vec<(LocalId, bool)> = binds.into_iter().collect();
    binds.sort();
    for (l, elem) in binds {
        let local = &f.body.locals[l.0 as usize];
        let changed = match held.contains(&l) {
            true => ev.deep.contains(&l) || ev.modified.contains(&l) && !ev.mutated.contains(&l),
            false => ev.mutated.contains(&l) || ev.modified.contains(&l) || ev.weak.contains(&l),
        };
        // Hidden locals of desugared loops (`<map@N>`) hold the source; the body can't name them.
        if local.name.starts_with('<') || !changed || cx.is_copy(local.ty) {
            continue;
        }
        let (name, tn, span) = (local.name.clone(), cx.display(local.ty), local.span);
        let what = match elem {
            true => "the loop changes",
            false => "this changes",
        };
        cx.error(
            Diagnostic::error(
                format!("{what} `{name}`, a `{tn}` read out of a `shared` value"),
                span,
            )
            .with_note(
                "a shared value is read-only, since other tasks read it at the same time: \
                 change a copy (`.slice()`, `structuredClone`), or share a `Mutex`",
            ),
        );
    }
}
