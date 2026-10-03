//! Call sites after each inference round: arguments follow the callee's current modes.
//!
//! - `Owned` param → the argument place is moved (or copied).
//! - `BorrowMut` param / receiver → the argument place is borrowed mutably (which makes its root
//!   modified in the caller: the next round propagates it). Module constants cannot be.
//! - Callbacks: a call that also passes a function value which may modify its arguments hands
//!   the callee a way to modify the other arguments. A closure literal or named function that
//!   modifies a non-Copy param makes every other argument borrowed mutably; a function value
//!   of unknown origin (a variable, param or field) conservatively does the same for arguments
//!   in their root's own memory (`p`, `p.f` — not array elements, which the callee's own
//!   exclusivity checks cover at the call site that knows the closure).
//!
//! Callee modes: `Callee::Def` / `New` the function's, `Virtual` the vtable entry's (joined over
//! overrides), `Dyn` / `ParamMethod` the interface method's (joined over implementations).

use std::collections::HashSet;

use velt_common::{Diagnostic, Span};

use super::infer::force_move;
use crate::body::places::{is_place, set_place_mode};
use crate::ctx::Ctx;
use crate::hir::{
    Block, Callee, Def, DefId, Expr, ExprKind as E, LocalId, PassMode, TyKind, UseMode,
};
use crate::visit;

/// Patch every call in `b`. `borrowed`: the body's borrowed function-typed params and captures
/// (`fn_values::borrowed_fn_locals`). Returns whether a use mode changed; errors are reported
/// once per span (`reported`).
pub(super) fn patch_calls(
    cx: &mut Ctx,
    b: &mut Block,
    borrowed: &HashSet<LocalId>,
    reported: &mut HashSet<Span>,
) -> bool {
    let mut changed = false;
    let mut errors = vec![];
    visit::exprs_mut(b, &mut |e: &mut Expr| {
        let (modes, args) = match &mut e.kind {
            E::Call { callee, args } => {
                if fixed_callee(cx, callee, args) {
                    let keeps = generic_params(cx, callee, args);
                    changed |= escape_literal_args(cx, args, &keeps, borrowed);
                }
                (call_modes(cx, callee, args), args)
            }
            E::New { def, args, .. } => match cx.adt(*def).and_then(|a| a.ctor) {
                Some(c) => (Some(modes_of(cx, c)[1..].to_vec()), args),
                None => return,
            },
            _ => return,
        };
        if let Some(modes) = &modes {
            for (a, m) in args.iter_mut().zip(modes) {
                match m {
                    PassMode::Owned => changed |= force_move(cx, a, &mut errors),
                    PassMode::BorrowMut => changed |= borrow_mut(cx, a, &mut errors),
                    _ => {}
                }
            }
        }
        if matches!(
            &e.kind,
            E::Call {
                callee: Callee::Intrinsic(_),
                ..
            }
        ) {
            return;
        }
        let (E::Call { args, .. } | E::New { args, .. }) = &mut e.kind else {
            return;
        };
        changed |= callbacks(cx, args, &mut errors);
    });
    for d in errors {
        let at = d.labels.first().map(|l| l.span);
        if at.is_none_or(|s| reported.insert(s)) {
            cx.diags.push(d);
        }
    }
    changed
}

/// Is the callee reached through a function value or a dynamically dispatched method? Its
/// parameter modes are fixed (borrowed), whatever it does with an argument: a generic `T` it
/// keeps may be instantiated with a function type.
fn fixed_callee(cx: &Ctx, callee: &Callee, args: &[Expr]) -> bool {
    match callee {
        Callee::Indirect(_) => true,
        Callee::Intrinsic(_) => false,
        _ => matches!(call_target(cx, callee, args), Some(Target::Iface(..))),
    }
}

/// A closure literal passed directly to a callee with fixed modes captures by value and owns a
/// heap environment: the callee may keep it (functions.md "Captures"). Not one that forwards a
/// borrowed function (captures one of `borrowed`) to a parameter of function type, which the
/// callee cannot keep: it stays in the caller's frame, as the callee's borrowed parameter. A
/// parameter declared with a generic type (`keep(x: T)`, `keeps[k]`) may be kept when `T` is a
/// function type, so a forwarding closure passed there escapes too: it captures the forwarded
/// function by value, which makes that parameter owned in turn. Returns whether a closure
/// changed.
fn escape_literal_args(
    cx: &mut Ctx,
    args: &[Expr],
    keeps: &[bool],
    borrowed: &HashSet<LocalId>,
) -> bool {
    let mut changed = false;
    for (k, a) in args.iter().enumerate() {
        if let E::Closure(def) = a.kind {
            let forwards = match &cx.defs[def.0 as usize] {
                Some(Def::Fn(c)) => c.captures.iter().any(|cap| borrowed.contains(&cap.outer)),
                _ => false,
            };
            if !forwards || keeps.get(k).copied().unwrap_or(false) {
                changed |= super::fn_values::escape_closure(cx, def);
            }
        }
    }
    changed
}

/// Per argument of a call with fixed modes (`this` first): is the callee's parameter declared
/// with a type that is not a function type (a generic `T` instantiated with one), so that the
/// callee may keep a function passed there?
fn generic_params(cx: &Ctx, callee: &Callee, args: &[Expr]) -> Vec<bool> {
    let Some(Target::Iface(iface, slot)) = call_target(cx, callee, args) else {
        return vec![];
    };
    let Some(m) = cx.iface(iface).and_then(|i| i.methods.get(slot as usize)) else {
        return vec![];
    };
    std::iter::once(false)
        .chain(m.params.iter().map(|p| !super::fn_values::is_fn(cx, p.ty)))
        .collect()
}

/// Where a call's callee modes come from, when known statically.
enum Target {
    Fn(DefId),
    Iface(DefId, u32),
}

fn call_target(cx: &Ctx, callee: &Callee, args: &[Expr]) -> Option<Target> {
    match callee {
        Callee::Def(d, _) => Some(Target::Fn(*d)),
        Callee::Virtual { slot } => {
            let (class, _) = cx.class_of(args.first()?.ty)?;
            let m = *cx.adt(class)?.vtable.get(*slot as usize)?;
            Some(Target::Fn(m))
        }
        Callee::Dyn { slot } => match cx.ty.kind(args.first()?.ty) {
            TyKind::Dyn(iface, _) => Some(Target::Iface(*iface, *slot)),
            _ => None,
        },
        Callee::ParamMethod { iface, slot, .. } => Some(Target::Iface(*iface, *slot)),
        _ => None,
    }
}

/// The modes of a call's callee (`this` first for methods), when known statically.
fn call_modes(cx: &Ctx, callee: &Callee, args: &[Expr]) -> Option<Vec<PassMode>> {
    match call_target(cx, callee, args)? {
        Target::Fn(d) => Some(modes_of(cx, d)),
        Target::Iface(iface, slot) => iface_modes(cx, iface, slot),
    }
}

/// The defs (functions, interfaces) whose modes patching call `e` reads (for
/// `super::worklist`).
pub(super) fn call_reads(cx: &Ctx, e: &Expr, out: &mut Vec<DefId>) {
    match &e.kind {
        E::Call { callee, args } => match call_target(cx, callee, args) {
            Some(Target::Fn(d) | Target::Iface(d, _)) => out.push(d),
            None => {}
        },
        E::New { def, .. } => out.extend(cx.adt(*def).and_then(|a| a.ctor)),
        _ => {}
    }
}

/// A function's modes, `this` first.
pub(super) fn modes_of(cx: &Ctx, d: DefId) -> Vec<PassMode> {
    let f = cx.fn_info(d);
    f.this
        .iter()
        .map(|t| t.mode)
        .chain(f.params.iter().map(|p| p.mode))
        .collect()
}

fn iface_modes(cx: &Ctx, iface: DefId, slot: u32) -> Option<Vec<PassMode>> {
    let m = cx.iface(iface)?.methods.get(slot as usize)?;
    let this = match m.mut_this {
        true => PassMode::BorrowMut,
        false => PassMode::Borrow,
    };
    Some(
        std::iter::once(this)
            .chain(m.params.iter().map(|p| p.mode))
            .collect(),
    )
}

/// Borrow argument place `e` mutably (through an upcast); returns whether a mode changed.
fn borrow_mut(cx: &Ctx, e: &mut Expr, errors: &mut Vec<Diagnostic>) -> bool {
    let target = match &mut e.kind {
        E::Upcast(inner) => &mut **inner,
        _ => e,
    };
    if !is_place(target) || outer_mode(target) != Some(UseMode::Borrow) {
        return false;
    }
    if let Some(name) = global_root(cx, target) {
        errors.push(
            Diagnostic::error(
                format!("cannot modify module-level constant `{name}`"),
                target.span,
            )
            .with_note("this call may modify it; call it on a local copy instead"),
        );
        return false;
    }
    set_place_mode(target, UseMode::BorrowMut);
    true
}

fn outer_mode(e: &Expr) -> Option<UseMode> {
    match e.kind {
        E::Local(_, m)
        | E::Field { mode: m, .. }
        | E::Index { mode: m, .. }
        | E::UnwrapSome(_, m)
        | E::UnwrapVariant { mode: m, .. } => Some(m),
        E::Global(_) => Some(UseMode::Borrow),
        _ => None,
    }
}

/// The module constant place `e` is rooted at, if any.
fn global_root(cx: &Ctx, e: &Expr) -> Option<String> {
    match &e.kind {
        E::Global(d) => Some(cx.global(*d).map(|g| g.name.clone()).unwrap_or_default()),
        E::Field { base, .. }
        | E::Index { base, .. }
        | E::UnwrapSome(base, _)
        | E::UnwrapVariant { expr: base, .. } => global_root(cx, base),
        _ => None,
    }
}

/// How a function-valued argument may modify its own arguments.
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
enum Callback {
    None,
    Unknown,
    Modifies,
}

fn callback(cx: &mut Ctx, a: &Expr) -> Callback {
    let TyKind::FnPtr { params, .. } = cx.ty.kind(a.ty).clone() else {
        return Callback::None;
    };
    if params.iter().all(|t| cx.is_copy(*t)) {
        return Callback::None;
    }
    let modifies = match &a.kind {
        E::Closure(def) => match &cx.defs[def.0 as usize] {
            Some(Def::Fn(c)) => c.params[c.captures.len()..]
                .iter()
                .any(|p| c.body.locals[p.local.0 as usize].mutable && p.mode != PassMode::Copy),
            _ => false,
        },
        E::FnRef(def, _) => cx
            .fn_info(*def)
            .params
            .iter()
            .any(|p| p.mode == PassMode::BorrowMut),
        _ => return Callback::Unknown,
    };
    match modifies {
        true => Callback::Modifies,
        false => Callback::None,
    }
}

/// The callback rule (module docs).
fn callbacks(cx: &mut Ctx, args: &mut [Expr], errors: &mut Vec<Diagnostic>) -> bool {
    let kinds: Vec<Callback> = args.iter().map(|a| callback(cx, a)).collect();
    let mut changed = false;
    for (i, arg) in args.iter_mut().enumerate() {
        let others = kinds
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, k)| *k)
            .max()
            .unwrap_or(Callback::None);
        let hit = match others {
            Callback::Modifies => true,
            Callback::Unknown => super::evidence::in_root_memory(arg),
            Callback::None => false,
        };
        // Only `Borrow` places change: Copy arguments are copied, receivers of any type
        // borrowed.
        if hit {
            changed |= borrow_mut(cx, arg, errors);
        }
    }
    changed
}
