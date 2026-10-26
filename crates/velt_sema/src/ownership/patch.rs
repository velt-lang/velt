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

use rustc_hash::FxHashSet;

use velt_common::{Diagnostic, Span};

use super::infer::force_move;
use crate::body::places::{frozen_root, is_place, set_place_mode};
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
    borrowed: &FxHashSet<LocalId>,
    reported: &mut FxHashSet<Span>,
) -> bool {
    let mut changed = false;
    let mut errors = vec![];
    let awaited = awaited_calls(b);
    // Only a program that reads a `shared` has values read out of one to check.
    let frozen = cx.frozen_reads;
    visit::exprs_mut(b, &mut |e: &mut Expr| {
        let returns_promise = matches!(cx.ty.kind(e.ty), TyKind::Promise(..));
        let span = e.span;
        let mut modified = 0u64;
        // The function called, and the index of its parameter the first argument fills.
        let mut shared_from = None;
        let (modes, args, unknown) = match &mut e.kind {
            E::Call { callee, args } => {
                // A closure literal passed to a parameter of a generic type may be kept as a
                // `T` (`forward<T>(sink, t)` handing it to a method that stores it), so it
                // escapes (`super::kept_fns`).
                let generic = super::kept_fns::generic_param_args(cx, callee, args);
                for (a, g) in args.iter().zip(&generic) {
                    if let (E::Closure(def), true) = (&a.kind, *g) {
                        changed |= super::fn_values::escape_closure(cx, *def);
                    }
                }
                if super::kept_fns::may_keep_args(cx, callee, args) {
                    // A promise the call returns may run the callee after this frame is gone
                    // (unless it is awaited right here): it may keep every argument.
                    let keeps = match returns_promise && !awaited.contains(&span) {
                        true => vec![true; args.len()],
                        false => generic_params(cx, callee, args),
                    };
                    changed |= escape_literal_args(cx, args, &keeps, borrowed);
                }
                let modes = call_modes(cx, callee, args);
                if let Some(Target::Fn(d)) = call_target(cx, callee, args) {
                    shared_from = Some((d, 0));
                }
                let unknown = modes.is_none() && !matches!(callee, Callee::Intrinsic(_));
                if let (true, Callee::Def(d, _)) = (frozen, &*callee) {
                    let info = cx.fn_info(*d);
                    modified = info.modified_params;
                }
                (modes, args, unknown)
            }
            E::New { def, args, .. } => match cx.adt(*def).and_then(|a| a.ctor) {
                Some(c) => {
                    // What the constructor changes of its arguments (`this` is bit 0).
                    if frozen {
                        modified = cx.fn_info(c).modified_params >> 1;
                    }
                    shared_from = Some((c, 1));
                    (Some(modes_of(cx, c)[1..].to_vec()), args, false)
                }
                None => return,
            },
            _ => return,
        };
        if unknown {
            table_args(cx, args, &mut errors);
        }
        if let Some(modes) = &modes {
            for (i, (a, m)) in args.iter_mut().zip(modes).enumerate() {
                // An argument the callee changes though it doesn't take it `BorrowMut`: one it
                // takes owned (a receiver it returns, a parameter it keeps), or a borrowed value
                // whose map entries a loop changes. Not one read out of a `shared`: the callee
                // would change a copy, where JavaScript changes the shared value.
                if modified != 0
                    && i < 64
                    && modified & (1 << i) != 0
                    && matches!(m, PassMode::Borrow | PassMode::Owned)
                    && frozen_part(cx, a)
                {
                    modifies_frozen(cx, a, &mut errors);
                    continue;
                }
                match m {
                    PassMode::Owned => changed |= force_move(cx, a, &mut errors),
                    PassMode::BorrowMut => changed |= borrow_mut(cx, a, &mut errors),
                    // A borrowed parameter the callee shares into what it keeps (`fill`).
                    PassMode::Borrow => {
                        if let Some((f, at)) = shared_from {
                            super::infer::kept_by_share(cx, f, at + i, a, &mut errors);
                        }
                    }
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
pub(super) fn fixed_callee(cx: &Ctx, callee: &Callee, args: &[Expr]) -> bool {
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
/// function type, and so may a callee whose result is a promise that is not awaited at once (it
/// may call the closure after this frame is gone): a forwarding closure passed there escapes too.
/// It captures the forwarded function by value, which makes that parameter owned in turn.
/// Returns whether a closure changed.
fn escape_literal_args(
    cx: &mut Ctx,
    args: &[Expr],
    keeps: &[bool],
    borrowed: &FxHashSet<LocalId>,
) -> bool {
    let mut changed = false;
    for (k, a) in args.iter().enumerate() {
        if let E::Closure(def) = a.kind {
            let forwards = match &cx.defs[def.0 as usize] {
                Some(Def::Fn(c)) => c.captures.iter().any(|cap| borrowed.contains(&cap.outer)),
                _ => false,
            };
            // A program that keeps borrowed function values lets no closure stay in a frame
            // where a callee with fixed modes may keep it (`super::kept_fns`).
            let kept = cx.kept_fns.on || keeps.get(k).copied().unwrap_or(false);
            if !forwards || kept {
                changed |= super::fn_values::escape_closure(cx, def);
            }
        }
    }
    changed
}

/// Spans of the calls in `b` that are awaited directly (`await f(...)`): their callee finishes
/// before the caller's frame does.
fn awaited_calls(b: &mut Block) -> FxHashSet<Span> {
    let mut out = FxHashSet::default();
    visit::exprs_mut(b, &mut |e: &mut Expr| {
        if let E::Await(inner) = &e.kind {
            if matches!(inner.kind, E::Call { .. }) {
                out.insert(inner.span);
            }
        }
    });
    out
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
pub(super) enum Target {
    Fn(DefId),
    Iface(DefId, u32),
}

pub(super) fn call_target(cx: &Ctx, callee: &Callee, args: &[Expr]) -> Option<Target> {
    match callee {
        Callee::Def(d, _) => Some(Target::Fn(*d)),
        Callee::Virtual { slot } => {
            let (class, _) = cx.class_of(args.first()?.ty)?;
            let m = *cx.adt(class)?.vtable.get(*slot as usize)?;
            Some(Target::Fn(m))
        }
        Callee::Dyn { slot, .. } => match cx.ty.kind(args.first()?.ty) {
            TyKind::Dyn(iface, _) => Some(Target::Iface(*iface, *slot)),
            _ => None,
        },
        Callee::ParamMethod { iface, slot, .. } => Some(Target::Iface(*iface, *slot)),
        _ => None,
    }
}

/// The modes of a call's callee (`this` first for methods), when known statically.
fn call_modes(cx: &mut Ctx, callee: &Callee, args: &[Expr]) -> Option<Vec<PassMode>> {
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
    let mut modes: Vec<PassMode> = f
        .this
        .iter()
        .map(|t| t.mode)
        .chain(f.params.iter().map(|p| p.mode))
        .collect();
    if f.kind == crate::defs::FnKind::Extern {
        if let Some(m) = extern_written_param(&f.name).and_then(|i| modes.get_mut(i)) {
            *m = PassMode::BorrowMut;
        }
    }
    modes
}

/// The parameter a runtime function (or a SIMD `store`) writes through, for the ones that fill
/// an array they are given. An extern's signature says nothing about it (its parameters are borrows), so without
/// this the std method that calls it would look like a reader: `K.fill(0)` on a module constant
/// would be accepted, and a table is read-only data.
fn extern_written_param(symbol: &str) -> Option<usize> {
    Some(match symbol {
        "velt_rt_bytes_set" | "velt_rt_bytes_copy_within" | "velt_rt_bytes_fill" => 0,
        "velt_rt_regex_exec" => 3,
        // `readInto(buf)` reads into the buffer's bytes.
        "velt_rt_tcp_try_read_into" => 1,
        // `velt_simd_<type>_store(v, to, at)` and `_store_partial(v, to, at, count)` (rt_abi.md
        // "SIMD"): `v.store(K)` modifies `K`.
        s if s.starts_with("velt_simd_")
            && (s.ends_with("_store") || s.ends_with("_store_partial")) =>
        {
            1
        }
        _ => return None,
    })
}

fn iface_modes(cx: &mut Ctx, iface: DefId, slot: u32) -> Option<Vec<PassMode>> {
    let i = cx.iface(iface)?;
    if let Some(k) = (slot as usize).checked_sub(i.methods.len() + i.fields.len()) {
        // A field setter (`crate::iface_writes`): it modifies the receiver and owns the value
        // (copies it for Copy types), as `collect::getters::field_setter` declares it.
        let f = *cx.setter_fields(iface).get(k)?;
        let ty = i.fields[f].ty;
        let v = match cx.is_copy(ty) {
            true => PassMode::Copy,
            false => PassMode::Owned,
        };
        return Some(vec![PassMode::BorrowMut, v]);
    }
    let m = i.methods.get(slot as usize)?;
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

/// Tables passed to a callee whose parameter modes are not known (a function value): it may
/// modify them, and they are read-only data.
/// A module constant passed to such a callee, which may modify it, is the one object the module
/// state keeps (`crate::kept_consts::keep`); a table that can't be kept is an error.
fn table_args(cx: &mut Ctx, args: &[Expr], errors: &mut Vec<Diagnostic>) {
    for a in args {
        let table = table_arg(cx, a);
        if !table && !object_global_arg(cx, a) {
            continue;
        }
        if keep_root(cx, a) || !table {
            continue;
        }
        let name = global_root(cx, a).unwrap_or_default();
        let msg = format!("cannot pass module constant `{name}` to a function value");
        errors.push(crate::body::pure_init::table_escapes(&name, msg, a.span));
    }
    // The same for a value read out of a `shared<T>`, which other tasks read at the same time.
    for a in args.iter().filter(|a| cx.frozen_reads && frozen_root(a)) {
        let tn = cx.display(a.ty);
        errors.push(
            Diagnostic::error(
                format!("cannot pass a `{tn}` read out of a `shared` value to a function value"),
                a.span,
            )
            .with_note("the function may modify it, and a shared value is read-only: pass a copy, or call a named function"),
        );
    }
}

/// Is argument `a` (through upcasts) a module constant, or a part of one, that a change could be
/// seen through (`crate::body::pure_init::mutable_identity`)?
fn object_global_arg(cx: &Ctx, a: &Expr) -> bool {
    let mut x = a;
    while let E::Upcast(inner) | E::Downcast(inner) = &x.kind {
        x = inner;
    }
    global_root(cx, x).is_some() && crate::body::pure_init::mutable_identity(cx, x.ty)
}

/// Is argument `a` a table (through upcasts)?
fn table_arg(cx: &Ctx, a: &Expr) -> bool {
    let mut x = a;
    while let E::Upcast(inner) | E::Downcast(inner) = &x.kind {
        x = inner;
    }
    matches!(x.kind, E::Global(d) if crate::body::pure_init::is_table(cx, d))
}

/// Borrow argument place `e` mutably (through an upcast); returns whether a mode changed.
fn borrow_mut(cx: &mut Ctx, e: &mut Expr, errors: &mut Vec<Diagnostic>) -> bool {
    let target = match &mut e.kind {
        E::Upcast(inner) | E::Downcast(inner) => &mut **inner,
        _ => e,
    };
    if cx.frozen_reads && frozen_part(cx, target) {
        modifies_frozen(cx, target, errors);
        return false;
    }
    if !is_place(target) || outer_mode(target) != Some(UseMode::Borrow) {
        return false;
    }
    // Module state may be modified in place: a module-level `let`, or a module constant the call
    // may modify, which is then the one object the module state keeps, as in Node.
    if keep_root(cx, target) {
        // The slot is the place itself.
        if matches!(target.kind, E::Global(_)) {
            return false;
        }
        set_place_mode(target, UseMode::BorrowMut);
        return true;
    }
    if let Some(name) = global_root(cx, target) {
        errors.push(
            Diagnostic::error(
                format!("cannot modify module-level constant `{name}`"),
                target.span,
            )
            .with_note(match table_arg(cx, target) {
                true => format!("this call may modify it, and a table is read-only data: use a copy, `{name}.slice()`"),
                false => "this call may modify it; call it on a local copy instead".to_string(),
            }),
        );
        return false;
    }
    set_place_mode(target, UseMode::BorrowMut);
    true
}

/// A call may modify `target`, a value read out of a `shared`: an error.
fn modifies_frozen(cx: &Ctx, target: &Expr, errors: &mut Vec<Diagnostic>) {
    let tn = cx.display(target.ty);
    errors.push(Diagnostic::error(
        format!("this call may modify its argument, a `{tn}` read out of a `shared` value"),
        target.span,
    ).with_note("a shared value is read-only, since other tasks read it at the same time: pass a copy, or share a `Mutex`"));
}

fn outer_mode(e: &Expr) -> Option<UseMode> {
    match e.kind {
        E::Local(_, m)
        | E::Field { mode: m, .. }
        | E::Index { mode: m, .. }
        | E::UnwrapSome(_, m)
        | E::UnwrapVariant { mode: m, .. } => Some(m),
        E::Downcast(ref x) => outer_mode(x),
        E::Global(_) => Some(UseMode::Borrow),
        _ => None,
    }
}

/// Is place `e` rooted at module state: a module-level `let`, or a module constant kept there
/// from now on (`crate::kept_consts::keep`)?
fn keep_root(cx: &mut Ctx, e: &Expr) -> bool {
    match &e.kind {
        E::Global(d) => crate::kept_consts::keep(cx, *d),
        E::Field { base, .. }
        | E::Index { base, .. }
        | E::UnwrapSome(base, _)
        | E::UnwrapVariant { expr: base, .. }
        | E::Upcast(base)
        | E::Downcast(base) => keep_root(cx, base),
        _ => false,
    }
}

/// The module constant place `e` is rooted at, if any.
fn global_root(cx: &Ctx, e: &Expr) -> Option<String> {
    match &e.kind {
        E::Global(d) => Some(cx.global(*d).map(|g| g.name.clone()).unwrap_or_default()),
        E::Field { base, .. }
        | E::Index { base, .. }
        | E::UnwrapSome(base, _)
        | E::UnwrapVariant { expr: base, .. }
        | E::Downcast(base) => global_root(cx, base),
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
            // A table can't be told apart from a copy here either: the function value may be
            // the one that modifies it.
            Callback::Unknown => {
                super::evidence::in_root_memory(arg)
                    || table_arg(cx, arg)
                    || object_global_arg(cx, arg)
                    || cx.frozen_reads && frozen_root(arg)
            }
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

/// Is `e` read out of a `shared`, or derived from such a read (`derived`)?
fn frozen_part(cx: &Ctx, e: &Expr) -> bool {
    super::derived::roots(cx, e).frozen
}
