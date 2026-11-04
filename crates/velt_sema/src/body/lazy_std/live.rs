//! The live set, and the hooks its constructs demand (#2161 step 2; velt_common
//! `hook_demand.rs`). Lowering builds what `main` reaches, the definitions it looks up by name,
//! and the glue of the types in that code; it may call a hook from any of it, std's code
//! included. *Live* code aims to cover that, and doesn't yet cover all of it (below): the
//! program's own definitions, the std
//! definitions lowering looks up by name ([`super::is_helper`], [`super::EAGER`]), the hooks
//! demanded so far, and, closed over, every definition live code names (a call, a function
//! value, a closure, a constant), the methods of each class or struct a live type mentions
//! (with its base classes, subclasses and fields' types, as [`super::used_types`]), and the
//! interface implementations of the live types. What [`super::reach`] checks only because
//! checked code names it (the prelude's methods, a module checked whole) is not live until
//! live code reaches it.
//!
//! Lowering also builds functions that are not live here (`T[].truncate`, the `arrayView*` hooks,
//! `errorText`, `uncaughtHeader`, `Generator.*`, the any-to-interface adapters; #2161). That is
//! harmless for the integer hooks only because any construct brings all of them through their
//! std code: this must be fixed before a hook with a narrower demand gets `constructs(…)`.
//!
//! The constructs of `hook_demand::Construct` are evaluated on live code with the operand's
//! type, and a hook one of whose constructs occurs is demanded: [`step`] adds it to what
//! [`super::reach`] checks. Each live definition is walked once, after it is checked.

use rustc_hash::{FxHashMap, FxHashSet};
use velt_common::std_hooks::{Construct, ConstructSet, HookSet, HOOKS};

use crate::ctx::Ctx;
use crate::defs::{DefInfo, FnKind};
use crate::hir::{self, Callee, DefId, ExprKind as E, Intrinsic as I, TyId, TyKind};

/// The live set of one check.
#[derive(Default)]
pub(crate) struct Live {
    /// The definitions below this one are seeded ([`seed`]; generated code is added later).
    seeded: usize,
    /// The live definitions.
    live: FxHashSet<DefId>,
    /// Live definitions still to walk (some not checked yet).
    todo: Vec<DefId>,
    /// The types met in live code.
    types: FxHashSet<TyId>,
    /// The classes and structs met in live code.
    adts: FxHashSet<DefId>,
    /// Per class or struct: all its methods and constructors (made when first needed).
    methods: Option<FxHashMap<DefId, Vec<DefId>>>,
    /// The implementations (`cx.impls` indexes) whose methods are live.
    impls: FxHashSet<usize>,
    /// The kinds of the live types an implementation or a gated root may be for ([`kind_bit`]).
    kinds: u32,
    /// The interfaces of the interface values in live code.
    ifaces: FxHashSet<DefId>,
    /// The functions lowering looks up by name that take a value only live code can give
    /// ([`gate`]), with what they take: live once live code has one.
    gated: Vec<(DefId, Vec<Gate>)>,
    /// The live classes, interfaces and kinds when [`open`] last ran.
    opened: Option<(usize, usize, u32)>,
    /// The definitions below this one had their std roots marked or gated ([`seed_std`]): once
    /// the program's own code is walked.
    std_seeded: usize,
    /// Every hook with constructs is demanded: nothing more to work out.
    saturated: bool,
    /// The sets of constructs that demand every hook, one bit per set ([`saturated`]).
    full: Option<u64>,
    /// The constructs met in live code.
    pub(super) present: ConstructSet,
    /// Per type: may its text or format print an integer ([`may_print_int`])?
    int_text: FxHashMap<TyId, bool>,
}

/// A value a function takes ([`gate`]): of a class or struct, an interface, or a kind of type
/// ([`kind_bit`]).
#[derive(Clone, Copy)]
enum Gate {
    Adt(DefId),
    Iface(DefId),
    Kind(u32),
}

/// What walking live code found.
#[derive(Default)]
struct Walk {
    refs: Vec<DefId>,
    types: Vec<TyId>,
    /// Operands converted to text (`ToString`, `TemplateToString`).
    texts: Vec<TyId>,
    /// Values formatted as `console.log` prints them.
    formats: Vec<TyId>,
    /// The interfaces whose methods it calls on a type parameter's value.
    ifaces: Vec<DefId>,
    /// The kinds ([`kind_bit`]) of its scalar types, which are not recorded as types.
    kinds: u32,
}

/// The hooks lowering may call (`hir::Program::hooks_demanded`): those the constructs of live
/// code demand; every hook when nothing is deferred (laziness off).
pub(crate) fn demanded(cx: &Ctx) -> HookSet {
    #[cfg(test)]
    tests::PRESENT.set((cx.lazy.live.present, cx.lazy.live.impls.len()));
    match cx.lazy.modules.is_empty() {
        true => HookSet::all(),
        false => HookSet::demanded_by(cx.lazy.live.present),
    }
}

/// Mark what is live since the last step, evaluate its constructs, and add the definitions of
/// the hooks they newly demand to `found`. Once every hook with constructs is demanded nothing
/// more can change: the live set is not worked out further (most programs that print get there
/// with their own code, before any std code is walked).
pub(super) fn step(cx: &mut Ctx, found: &mut Vec<DefId>) {
    if cx.lazy.live.saturated {
        return;
    }
    if cx.lazy.live.seeded < cx.info.len() {
        seed(cx);
    }
    let before = cx.lazy.live.present;
    let mut pending = vec![];
    loop {
        if saturated(cx) {
            break;
        }
        let todo = std::mem::take(&mut cx.lazy.live.todo);
        if todo.is_empty() {
            // The program's code walked: then what lowering looks up by name, and what the
            // live types, interfaces and kinds open.
            if cx.lazy.live.std_seeded < cx.info.len() {
                seed_std(cx);
            }
            open(cx);
            if cx.lazy.live.todo.is_empty() {
                break;
            }
            continue;
        }
        let mut w = Walk::default();
        for d in todo {
            if !walk_def(cx, d, &mut w) {
                pending.push(d);
            }
        }
        evaluate(cx, &w);
        if saturated(cx) {
            break;
        }
        cx.lazy.live.kinds |= w.kinds;
        cx.lazy.live.ifaces.extend(w.ifaces);
        for d in w.refs {
            mark(cx, d);
        }
        let mut types = w.types;
        types.extend(w.formats.iter().chain(&w.texts));
        live_types(cx, types);
    }
    // Not checked yet: walked once checked. `reach` checks what checked code names and the
    // methods of the types it uses, which covers what is live; a hook lowering calls only where
    // sema checked it (`RECORD_HOOKS`, the float hooks) may stay unchecked.
    cx.lazy.live.todo.extend(pending);
    let now = cx.lazy.live.present;
    if now == before {
        return;
    }
    let (old, new) = (HookSet::demanded_by(before), HookSet::demanded_by(now));
    for (i, h) in HOOKS.iter().enumerate() {
        if new.contains(i) && !old.contains(i) {
            if let Some(crate::ctx::Item::Def(d)) = cx.std_def(&h.qualified()) {
                mark(cx, d);
                found.push(d);
            }
        }
    }
}

/// Does every hook with constructs have one present? Then nothing more is worked out
/// (`Live::saturated`).
fn saturated(cx: &mut Ctx) -> bool {
    let live = &mut cx.lazy.live;
    if !live.saturated {
        // Which sets of constructs demand every hook, one bit per set.
        let full = *live.full.get_or_insert_with(|| {
            (0..1u32 << Construct::ALL.len())
                .filter(|&p| velt_common::std_hooks::all_demanded(p as ConstructSet))
                .fold(0u64, |m, p| m | 1 << p)
        });
        live.saturated = full & (1 << live.present) != 0;
    }
    live.saturated
}

/// The roots: the program's own definitions (its root module's, whether or not in std), and,
/// walked after them ([`seed_std`]), the free functions lowering looks up by name: the helpers
/// checked with the prelude, those of the modules it looks into ([`super::EAGER`]), and the
/// hooks no construct lists (some checked on demand: live once checked). Such a function that
/// takes a value only live code can give (an array, a class instance, an `any`) is live once
/// live code has a value of that type ([`gate`]). Methods are lowering's only where it has a
/// value of their type: live with it.
fn seed(cx: &mut Ctx) {
    let from = std::mem::replace(&mut cx.lazy.live.seeded, cx.info.len());
    // Generated code may add methods.
    cx.lazy.live.methods = None;
    // The root module is the program's even in std (a source generator's).
    let program: Vec<bool> = (cx.modules.iter().enumerate())
        .map(|(m, sm)| !sm.is_std || m == cx.root)
        .collect();
    let any_live_type = !cx.lazy.live.adts.is_empty();
    for i in from..cx.info.len() {
        let (module, owner) = match &cx.info[i] {
            DefInfo::Fn(f) => (f.module, f.owner),
            DefInfo::Global(g) => (g.module, None),
            _ => continue,
        };
        let of_live = any_live_type && owner.is_some_and(|o| cx.lazy.live.adts.contains(&o));
        if program[module] || of_live {
            mark(cx, DefId(i as u32));
        }
    }
}

/// The std roots of [`seed`] (`Live::std_seeded` on): marked, or gated ([`gate`]).
fn seed_std(cx: &mut Ctx) {
    let from = std::mem::replace(&mut cx.lazy.live.std_seeded, cx.info.len());
    let hooks: FxHashSet<DefId> = (HOOKS.iter())
        .filter(|h| h.lowered && h.constructs.is_empty())
        .filter_map(|h| match cx.std_def(&h.qualified()) {
            Some(crate::ctx::Item::Def(d)) => Some(d),
            _ => None,
        })
        .collect();
    for i in from..cx.info.len() {
        let d = DefId(i as u32);
        let DefInfo::Fn(f) = &cx.info[i] else {
            continue;
        };
        let m = &cx.modules[f.module];
        // A method or a static method is live with its type; a closure with its function.
        if !m.is_std
            || f.module == cx.root
            || f.owner.is_some()
            || !matches!(f.kind, FnKind::Free | FnKind::Static)
        {
            continue;
        }
        let named = match cx.lazy.modules[f.module] {
            // The helpers: the free functions not deferred there.
            super::Mode::Helpers => !super::deferred(cx, d),
            super::Mode::Eager => super::EAGER.iter().any(|p| m.path.starts_with(p)),
            super::Mode::Lazy => false,
        };
        if named || hooks.contains(&d) {
            match gate(cx, f) {
                gates if gates.is_empty() => mark(cx, d),
                gates => cx.lazy.live.gated.push((d, gates)),
            }
        }
    }
}

/// What a function lowering calls by name takes or gives that only live code has: the
/// classes, structs and interfaces its signature mentions, or failing those, the kinds (an
/// array, a map, an `any`, a function) in it. Lowering calls `stableSort` for an array's
/// `sort()`, `uncaughtHeader` for an `Error` thrown, `anyArr_push` for an `any`, `classObject` to
/// print a class object (its result mentions `ClassMarker`). Empty when it has only numbers,
/// strings and the like, or type parameters: always live. The functions of `velt/sync/epoch` take
/// nothing: lowering calls them for a reference held by an `Atomic`.
fn gate(cx: &Ctx, f: &crate::defs::FnInfo<'_>) -> Vec<Gate> {
    let sig = (f.this.iter().map(|t| t.ty))
        .chain(f.params.iter().map(|p| p.ty))
        .chain([f.ret]);
    let (mut heads, mut kinds, mut seen) = (vec![], vec![], FxHashSet::default());
    let mut todo: Vec<TyId> = sig.collect();
    while let Some(t) = todo.pop() {
        if !seen.insert(t) {
            continue;
        }
        let kind = cx.ty.kind(t);
        match kind {
            TyKind::Adt(d, _) => heads.push(Gate::Adt(*d)),
            TyKind::Dyn(d, _) if cx.dynamic(t).is_none() => heads.push(Gate::Iface(*d)),
            TyKind::Dyn(..) => kinds.push(Gate::Kind(DYNAMIC)),
            k if kind_bit(k) & GATE_KINDS != 0 => kinds.push(Gate::Kind(kind_bit(k))),
            _ => {}
        }
        todo.extend(crate::types::children(kind));
    }
    let module = &cx.modules[f.module].path;
    if module.starts_with(velt_common::std_module!(sync_epoch)) {
        heads.extend(cx.prelude_adt("Atomic").map(Gate::Adt));
    }
    match heads.is_empty() {
        true => kinds,
        false => heads,
    }
}

/// The implementations and gated roots that the live classes, interfaces and kinds open, when
/// those changed since the last time.
fn open(cx: &mut Ctx) {
    let live = &cx.lazy.live;
    let now = (live.adts.len(), live.ifaces.len(), live.kinds);
    if live.opened == Some(now) {
        return;
    }
    cx.lazy.live.opened = Some(now);
    live_impls(cx);
    open_gates(cx);
}

/// The gated roots one of whose values live code now has.
fn open_gates(cx: &mut Ctx) {
    let gated = std::mem::take(&mut cx.lazy.live.gated);
    let live = &cx.lazy.live;
    let open = |g: &Gate| match *g {
        Gate::Adt(d) => live.adts.contains(&d),
        Gate::Iface(d) => live.ifaces.contains(&d),
        Gate::Kind(bit) => live.kinds & bit != 0,
    };
    let (now, still): (Vec<_>, Vec<_>) = gated
        .into_iter()
        .partition(|(_, gates)| gates.iter().any(open));
    cx.lazy.live.gated = still;
    for (d, _) in now {
        mark(cx, d);
    }
}

fn mark(cx: &mut Ctx, d: DefId) {
    if cx.lazy.live.live.insert(d) {
        cx.lazy.live.todo.push(d);
    }
}

/// Walk the checked code of live definition `d` into `w`; false if it is not checked yet.
fn walk_def(cx: &mut Ctx, d: DefId, w: &mut Walk) -> bool {
    let i = d.0 as usize;
    let ty = &cx.ty;
    match (&mut cx.defs[i], &mut cx.info[i]) {
        (Some(hir::Def::Fn(f)), info) => {
            if let DefInfo::Fn(fi) = info {
                for e in fi.params.iter_mut().filter_map(|p| p.default.as_mut()) {
                    crate::visit::exprs_in(e, &mut |e| walk_expr(ty, e, w));
                }
                // Thrown and not caught, it is reported as `console.log` prints it: lowering
                // builds that from the signature, whatever the body throws. (`hir::FnDef::throws`
                // is filled in later, by `finalize::build_defs`.)
                w.formats
                    .extend(fi.declared_throws.as_ref().and_then(|d| d.ty));
            }
            for t in f.body.locals.iter().map(|l| l.ty).chain([f.ret]) {
                note_ty(ty, t, w);
            }
            crate::visit::exprs_mut(&mut f.body.block, &mut |e| walk_expr(ty, e, w));
            true
        }
        (None, DefInfo::Fn(_)) => false,
        (_, DefInfo::Global(g)) => {
            if g.state != crate::defs::BodyState::Done {
                return false;
            }
            note_ty(ty, g.ty, w);
            if let Some(e) = &mut g.init {
                crate::visit::exprs_in(e, &mut |e| walk_expr(ty, e, w));
            }
            true
        }
        _ => true,
    }
}

/// Record type `t` of live code: a scalar by its kind only (it opens implementations and has
/// no parts), anything else to [`live_types`].
#[inline]
fn note_ty(ty: &crate::types::Types, t: TyId, w: &mut Walk) {
    match ty.kind(t) {
        k @ (TyKind::Int(_)
        | TyKind::Float(_)
        | TyKind::Bool
        | TyKind::Str
        | TyKind::Symbol
        | TyKind::Simd(_)
        | TyKind::Literal(_)) => w.kinds |= kind_bit(k),
        TyKind::Unit | TyKind::Never | TyKind::Error => {}
        _ => {
            if w.types.last() != Some(&t) {
                w.types.push(t);
            }
        }
    }
}

fn walk_expr(ty: &crate::types::Types, e: &hir::Expr, w: &mut Walk) {
    note_ty(ty, e.ty, w);
    match &e.kind {
        E::Call {
            callee: Callee::Def(d, targs),
            ..
        }
        | E::FnRef(d, targs) => {
            w.refs.push(*d);
            w.types.extend(targs);
        }
        E::Global(d) | E::Closure(d) => w.refs.push(*d),
        E::AdtLit { type_args, .. } | E::Variant { type_args, .. } | E::New { type_args, .. } => {
            w.types.extend(type_args)
        }
        E::ToDyn { expr, .. } => w.formats.push(expr.ty),
        E::Throw(v) => w.formats.push(v.ty),
        E::Call {
            callee: Callee::Intrinsic(I::ToString | I::TemplateToString),
            args,
        } => w.texts.extend(args.iter().map(|a| a.ty)),
        E::Call {
            callee: Callee::Intrinsic(I::Print | I::PrintErr | I::Inspect | I::ErrorText),
            args,
        } => w.formats.extend(args.iter().map(|a| a.ty)),
        E::Call {
            callee: Callee::Dyn {
                method_type_args, ..
            },
            ..
        } => w.types.extend(method_type_args),
        E::Call {
            callee:
                Callee::ParamMethod {
                    iface,
                    method_type_args,
                    iface_args,
                    ..
                },
            ..
        } => {
            w.ifaces.push(*iface);
            w.types.extend(method_type_args.iter().chain(iface_args));
        }
        _ => {}
    }
}

/// The constructs of `w`'s conversions and formatted values.
fn evaluate(cx: &mut Ctx, w: &Walk) {
    let mut present = 0;
    for &t in &w.texts {
        present |= match cx.ty.kind(t) {
            TyKind::Int(it) if it.is_signed() => Construct::TextInt.bit(),
            TyKind::Int(_) => Construct::TextUint.bit(),
            // Floats print through the float hooks (`number_text::reach_hooks`).
            TyKind::Float(_) | TyKind::Bool | TyKind::Str => 0,
            // An instance may be an integer of either sign.
            TyKind::Param(_) | TyKind::Op(..) => {
                Construct::TextInt.bit() | Construct::TextUint.bit() | Construct::Format.bit()
            }
            _ => format_bit(cx, t),
        };
    }
    for &t in &w.formats {
        present |= format_bit(cx, t);
    }
    cx.lazy.live.present |= present;
}

fn format_bit(cx: &mut Ctx, t: TyId) -> ConstructSet {
    match may_print_int(cx, t) {
        true => Construct::Format.bit(),
        false => 0,
    }
}

/// May lowering's text or formatting glue for a value of type `t` print an integer: is it, or
/// may it hold, anything but a float, a boolean, a string, a symbol, `undefined` or a function?
/// An array prints its length past 100 elements, a map its size, a class its fields (and may
/// be a subclass's instance), an enum its discriminant; `any` and a type parameter may hold
/// anything.
fn may_print_int(cx: &mut Ctx, t: TyId) -> bool {
    if let Some(&known) = cx.lazy.live.int_text.get(&t) {
        return known;
    }
    // A recursive type comes back here through its children: no integer so far.
    cx.lazy.live.int_text.insert(t, false);
    let kind = cx.ty.kind(t).clone();
    let may = match &kind {
        TyKind::Float(_)
        | TyKind::Bool
        | TyKind::Str
        | TyKind::Unit
        | TyKind::Never
        | TyKind::Error
        | TyKind::Symbol
        | TyKind::FnPtr { .. }
        | TyKind::Closure(_)
        | TyKind::Literal(hir::LitValue::Str(_) | hir::LitValue::Bool(_)) => false,
        TyKind::Tuple(_)
        | TyKind::Option(..)
        | TyKind::Result(..)
        | TyKind::Promise(..)
        | TyKind::Shared(_) => {
            (crate::types::children(&kind).into_iter()).any(|c| may_print_int(cx, c))
        }
        _ => true,
    };
    cx.lazy.live.int_text.insert(t, may);
    may
}

/// Record the types `todo` and their parts as live: their constructs, and the methods of each
/// class or struct among them (with its base classes, subclasses and fields' types), and of the
/// implementations for them.
fn live_types(cx: &mut Ctx, mut todo: Vec<TyId>) {
    let mut adts: Vec<DefId> = vec![];
    loop {
        if let Some(t) = todo.pop() {
            if !cx.lazy.live.types.insert(t) {
                continue;
            }
            let kind = cx.ty.kind(t).clone();
            cx.lazy.live.kinds |= kind_bit(&kind);
            match kind {
                TyKind::Dyn(..) if cx.dynamic(t).is_some() => cx.lazy.live.kinds |= DYNAMIC,
                TyKind::Dyn(d, _) => _ = cx.lazy.live.ifaces.insert(d),
                _ => {}
            }
            let bit = match &kind {
                TyKind::Array(_) => Construct::IndexCheck.bit(),
                TyKind::Dyn(..) if cx.dynamic(t).is_some() => Construct::Dynamic.bit(),
                // The `format` slot of its table (`velt_vir` `glue/vtable.rs`).
                TyKind::Adt(..) | TyKind::Dyn(..) => format_bit(cx, t),
                // A rejected promise not handled is reported with its error.
                TyKind::Promise(_, e) | TyKind::Result(_, e) => format_bit(cx, *e),
                _ => 0,
            };
            if cx.lazy.live.present | bit != cx.lazy.live.present {
                cx.lazy.live.present |= bit;
                if saturated(cx) {
                    return;
                }
            }
            todo.extend(crate::types::children(&kind));
            if let TyKind::Adt(d, _) = kind {
                adts.push(d);
            }
            continue;
        }
        let Some(d) = adts.pop() else { break };
        if !cx.lazy.live.adts.insert(d) {
            continue;
        }
        if let Some(a) = cx.adt(d) {
            todo.extend(a.fields.iter().map(|f| f.ty));
            todo.extend(a.base);
        }
        adts.extend(cx.lazy.subclasses.get(&d).into_iter().flatten().copied());
        let ms = methods(cx).get(&d).cloned().unwrap_or_default();
        for m in ms {
            mark(cx, m);
        }
        defaults(cx, d);
    }
}

/// The methods and constructors of every class and struct, by owner.
fn methods<'a>(cx: &'a mut Ctx) -> &'a FxHashMap<DefId, Vec<DefId>> {
    if cx.lazy.live.methods.is_none() {
        let mut by: FxHashMap<DefId, Vec<DefId>> = FxHashMap::default();
        for (i, info) in cx.info.iter().enumerate() {
            if let DefInfo::Fn(f) = info {
                if let Some(o) = f.owner {
                    by.entry(o).or_default().push(DefId(i as u32));
                }
            }
        }
        cx.lazy.live.methods = Some(by);
    }
    cx.lazy
        .live
        .methods
        .as_ref()
        .expect("ICE: methods by owner")
}

/// The field defaults and redefined initializers of live class or struct `d` (its constructor
/// runs them).
fn defaults(cx: &mut Ctx, d: DefId) {
    let mut w = Walk::default();
    let ty = &cx.ty;
    if let DefInfo::Adt(a) = &mut cx.info[d.0 as usize] {
        for e in a.fields.iter_mut().filter_map(|f| f.default.as_mut()) {
            crate::visit::exprs_in(e, &mut |e| walk_expr(ty, e, &mut w));
        }
        for e in a.redefined.iter_mut().filter_map(|r| r.init.as_mut()) {
            crate::visit::exprs_in(e, &mut |e| walk_expr(ty, e, &mut w));
        }
    }
    evaluate(cx, &w);
    cx.lazy.live.kinds |= w.kinds;
    cx.lazy.live.ifaces.extend(w.ifaces);
    for r in w.refs {
        mark(cx, r);
    }
    let mut types = w.types;
    types.extend(w.formats.iter().chain(&w.texts));
    if !types.is_empty() {
        live_types(cx, types);
    }
}

/// The implementations of a live interface for a live type: their methods are (lowering calls
/// them through a bound or a table). An interface is live where live code holds a value of it or
/// calls its methods on a type parameter's value; every one is once live code holds an `any`,
/// whose tables reach them. A type parameter's implementation (`impl<T> any for T`) is for every
/// type.
fn live_impls(cx: &mut Ctx) {
    let mut marked = vec![];
    let live = &cx.lazy.live;
    for (k, imp) in cx.impls.iter().enumerate() {
        if live.impls.contains(&k)
            || !(live.kinds & DYNAMIC != 0 || live.ifaces.contains(&imp.iface))
        {
            continue;
        }
        let for_live_type = match cx.ty.kind(imp.ty) {
            TyKind::Adt(d, _) => live.adts.contains(d),
            k => match kind_bit(k) {
                0 => true,
                bit => live.kinds & bit != 0,
            },
        };
        if for_live_type {
            marked.push((k, imp.methods.clone()));
        }
    }
    for (k, ms) in marked {
        cx.lazy.live.impls.insert(k);
        for m in ms {
            mark(cx, m);
        }
    }
}

/// The bit of a kind of type an implementation may be for, other than a class or struct (0 for
/// the others).
fn kind_bit(kind: &TyKind) -> u32 {
    match kind {
        TyKind::Int(it) | TyKind::Literal(hir::LitValue::Int(it, _)) => 1 << (*it as u32 % 16),
        TyKind::Float(_) | TyKind::Literal(hir::LitValue::Float(..)) => 1 << 16,
        TyKind::Bool | TyKind::Literal(hir::LitValue::Bool(_)) => 1 << 17,
        TyKind::Str | TyKind::Literal(hir::LitValue::Str(_)) => 1 << 18,
        TyKind::Symbol => 1 << 19,
        TyKind::Simd(_) => 1 << 20,
        TyKind::Array(_) => 1 << 21,
        TyKind::Map(..) => 1 << 22,
        TyKind::Dyn(..) => 1 << 27,
        TyKind::Promise(..) => 1 << 24,
        TyKind::Tuple(_) => 1 << 25,
        TyKind::FnPtr { .. } | TyKind::Closure(_) => 1 << 26,
        _ => 0,
    }
}

/// The kinds of [`kind_bit`] that gate a root ([`gate`]): values lowering does not make up.
const GATE_KINDS: u32 = (1 << 21) | (1 << 22) | DYNAMIC | (1 << 24) | (1 << 25) | (1 << 26);

/// The bit of a dynamic type (`any`, `unknown`, `object`, `Function`) among [`kind_bit`]'s.
const DYNAMIC: u32 = 1 << 23;

#[cfg(test)]
#[path = "live_tests.rs"]
mod tests;
