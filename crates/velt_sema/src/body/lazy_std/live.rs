//! The live set, what checks a std body, and the hooks its constructs demand (#2161 step 2;
//! velt_common `hook_demand.rs`). Lowering builds what `main` reaches, the definitions it looks up
//! by name, and the glue of the types in that code; it may call a hook from any of it, std's code
//! included. *Live* code is a superset of the functions it builds: the program's own definitions,
//! the std definitions lowering looks up by name ([`super::Mode::named`]), the hooks demanded so
//! far, and, closed over, every definition live code names (a call, a function value, a closure,
//! a constant), the methods of each class or struct a live type mentions (with its base classes,
//! subclasses and fields' types: a class value's glue dispatches over every class that extends
//! it, constructed or not, `velt_vir` `any_subclass_dispatch`, `json_descendants`), and the
//! interface implementations of the live types, and every module slot, whose initializer lowering
//! runs whether or not code reads it. A type argument of a checked call is a live type:
//! monomorphization builds the methods the callee calls on it. Three more rules are about what
//! lowering makes that no code in the program names: a generator function's value is a `Generator`
//! or `AsyncGenerator`, whatever its written result says; a call of an async host function is
//! awaited through a `HostSettled`; an `any` may become an `ArrayView` and may hold an `Error`
//! ([`dynamic_classes`]); and what sema makes once the bodies are checked (the implementations
//! of interfaces through `any`, the conversions of its functions, the adapters of `toString`
//! and `toJSON`) is added by [`finish`].
//!
//! [`step`] checks each deferred definition as it enters the set, then walks its code once: being
//! live is what gets a std body checked, so a gap in these rules leaves a stub that lowering
//! refuses to build (`hir::Program::unchecked`). A definition that is checked but not live (one
//! a checked call asked for, or in a module checked whole) adds nothing here. A debug compiler
//! (the one the goldens run), or `VELT_LIVE_CHECK=1`, also gives the set to lowering
//! (`hir::Program::live`), which asserts that each function it builds, and each constant or module
//! slot it reads, is in it (`Cx::assert_live`): what is checked and built but not live could have
//! a construct no hook was demanded for. One thing a release compiler doesn't walk: the forwarders
//! [`live_defs`] names.
//!
//! The constructs of `hook_demand::Construct` are evaluated on live code with the operand's
//! type. A construct present demands the integer hooks, all of them
//! ([`constructs::demanded_for`]), and they are live.

use rustc_hash::{FxHashMap, FxHashSet};
use velt_common::std_hooks::{hook_index, Construct, ConstructSet, HookSet, HOOKS};

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{self, Callee, DefId, ExprKind as E, Intrinsic as I, TyId, TyKind};

/// The live set of one check.
#[derive(Default)]
pub(crate) struct Live {
    /// Give the set to lowering to check against it (`hir::Program::live`).
    check: bool,
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
    /// The hooks that wait for what the program does ([`super::RECORD_HOOKS`], the float hooks):
    /// live, but checked only when [`super::reach`] finds they are needed. Lowering does without
    /// them until then.
    waiting: FxHashSet<DefId>,
    /// The operands of the text conversions in the program's own code and in the field defaults
    /// of live classes, since [`step`] last gave them.
    texts: Vec<TyId>,
    /// [`dynamic_classes`] ran.
    dynamic_classes: bool,
    /// [`finish`] began: no std module can be added any more (`Ctx::demand_std`).
    pub(crate) finishing: bool,
    /// The constructs met in live code.
    pub(super) present: ConstructSet,
    /// Definitions live from the start ([`assume`]), until they are declared.
    assumed: Vec<(DefId, String)>,
    /// What [`finish`] found too late, for the next round ([`too_late`]).
    pub(super) late: Assumed,
    /// The constructs whose hooks are demanded so far ([`demand_hooks`]).
    demanded: ConstructSet,
    /// Per type: may its text or format print an integer ([`may_print_int`])?
    int_text: FxHashMap<TyId, bool>,
}

impl Live {
    /// `VELT_LIVE_CHECK`, `1` or `0`, says whether to give the set to lowering to check against;
    /// a debug compiler does.
    pub(super) fn new() -> Live {
        let check = match std::env::var_os("VELT_LIVE_CHECK") {
            Some(v) => v != "0",
            None => cfg!(debug_assertions),
        };
        Live {
            check,
            ..Live::default()
        }
    }
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
    /// The types its explicit conversions of an `any` give (`AnyTo`).
    conversions: Vec<TyId>,
    /// The `(value, interface)` types of its conversions to an interface value (`ToDyn`).
    into_any: Vec<(TyId, TyId)>,
    /// Values formatted as `console.log` prints them.
    formats: Vec<TyId>,
    /// `console.log` or `console.error` of a float.
    prints_float: bool,
    /// The interfaces whose methods it calls on a type parameter's value.
    ifaces: Vec<DefId>,
    /// The kinds ([`kind_bit`]) of its scalar types, which are not recorded as types.
    kinds: u32,
    /// Generator functions, and async ones: their values are `Generator`s and `AsyncGenerator`s
    /// whatever their written result says.
    generators: [bool; 2],
    /// An async host function: lowering awaits its call's `HostSettled` (`velt_vir` `host.rs`).
    async_host: bool,
    /// A call of `catchUnwind` ([`Construct::CatchUnwind`]).
    catch_unwind: bool,
    /// The last type added to `types` (a body repeats the same type many times in a row).
    last_type: Option<TyId>,
}

impl Walk {
    /// Where one definition's types and conversions start in this walk.
    fn start(&mut self) -> (usize, usize, usize, usize) {
        self.last_type = None;
        let lens = (self.types.len(), self.conversions.len());
        (lens.0, lens.1, self.into_any.len(), self.texts.len())
    }
}

/// The hooks lowering may call (`hir::Program::hooks_demanded`): those the constructs of live
/// code demand; when nothing is deferred (laziness off), every hook but std's panic path, which
/// only a live `catchUnwind` asks for and laziness off doesn't look for (its programs then panic
/// through the runtime, as every program did before; the VIR stays the same either way).
pub(crate) fn demanded(cx: &Ctx) -> HookSet {
    #[cfg(test)]
    tests::note_demanded(cx);
    match cx.lazy.modules.is_empty() {
        true => {
            let mut all = HookSet::default();
            let catch_only = |h: &velt_common::std_hooks::Hook| {
                h.constructs == [Construct::CatchUnwind].as_slice()
            };
            for (i, _) in HOOKS.iter().enumerate().filter(|(_, h)| !catch_only(h)) {
                all.insert(i);
            }
            all
        }
        false => HookSet::demanded_by(demanded_for(cx.lazy.live.present)),
    }
}

/// Start this check with what an earlier round learned too late ([`too_late`]).
pub(crate) fn assume(cx: &mut Ctx, assumed: &Assumed) {
    cx.lazy.live.present = assumed.constructs;
    cx.lazy.live.assumed = assumed.defs.clone();
    cx.lazy.record_conversion = assumed.record_conversion;
}

/// The live definitions for lowering to check against (`hir::Program::live`), when it does, with
/// the definitions made since [`finish`]: the forwarders of an interface converted to another
/// (`Ctx::upcast_tables`), made once ownership is settled, so that a hook can no longer be
/// checked. Each reads a field or a getter and calls what it holds, so they demand nothing; this
/// asserts it. A release compiler doesn't walk them.
pub(crate) fn live_defs(cx: &mut Ctx, unchecked: &FxHashSet<DefId>) -> Option<FxHashSet<DefId>> {
    if !cx.lazy.live.check || cx.lazy.modules.is_empty() {
        return None;
    }
    // A stub has a signature and no code. Not walked, and not live.
    let live = &mut cx.lazy.live;
    live.todo.retain(|d| !unchecked.contains(d));
    live.live.extend(unchecked);
    live.opened = None;
    walk_set(cx);
    let (before, now) = (cx.lazy.live.demanded, cx.lazy.live.present);
    if let Some(h) = newly_demanded(before, now).next() {
        panic!(
            "ICE: a definition made after ownership demands the hook `{}`",
            h.name
        );
    }
    Some(cx.lazy.live.live.difference(unchecked).copied().collect())
}

/// Work out what is live since the last step, checking what enters the set, and demand and check
/// the hooks its constructs need, and what they reach. Gives the operands of the text conversions
/// that count for the float hooks ([`note_program_code`]).
pub(super) fn step(cx: &mut Ctx) -> Vec<TyId> {
    loop {
        walk_set(cx);
        if !demand_hooks(cx) {
            return std::mem::take(&mut cx.lazy.live.texts);
        }
    }
}

/// The hooks that the constructs `now` demand and those of `before` don't.
fn newly_demanded(
    before: ConstructSet,
    now: ConstructSet,
) -> impl Iterator<Item = &'static velt_common::std_hooks::Hook> {
    let demanded = |set| HookSet::demanded_by(demanded_for(set));
    let (old, new) = (demanded(before), demanded(now));
    let new_hook = move |&(i, _): &(usize, _)| new.contains(i) && !old.contains(i);
    HOOKS.iter().enumerate().filter(new_hook).map(|(_, h)| h)
}

/// Mark the hooks newly demanded by the constructs met: whether any was. Once the passes have
/// run they are left to the next round ([`too_late`]).
fn demand_hooks(cx: &mut Ctx) -> bool {
    let now = cx.lazy.live.present;
    let before = std::mem::replace(&mut cx.lazy.live.demanded, now);
    #[cfg(test)]
    if before == 0 && now != 0 {
        tests::note_first_demand(now);
    }
    if cx.lazy.live.finishing {
        if newly_demanded(before, now).next().is_some() {
            cx.lazy.live.late.constructs |= now;
        }
        return false;
    }
    let mut marked = false;
    for h in newly_demanded(before, now) {
        cx.demand_std(&h.module());
        if let Some(crate::ctx::Item::Def(d)) = cx.std_def(&h.qualified()) {
            marked = true;
            mark(cx, d);
        }
    }
    marked
}

/// Check and walk what is live until nothing new is.
fn walk_set(cx: &mut Ctx) {
    let mut pending = vec![];
    loop {
        let todo = std::mem::take(&mut cx.lazy.live.todo);
        if todo.is_empty() {
            // The roots, and what the live types, interfaces and kinds open.
            if cx.lazy.live.seeded < cx.info.len() {
                seed(cx);
            }
            assumed_live(cx);
            open(cx);
            if cx.lazy.live.todo.is_empty() {
                break;
            }
            continue;
        }
        let mut w = Walk::default();
        for d in todo {
            if !cx.lazy.live.waiting.contains(&d) {
                super::check_found(cx, d);
            }
            let start = w.start();
            if !walk_def(cx, d, &mut w) {
                pending.push(d);
            } else if program_code(cx, d) {
                note_program_code(cx, &w, start);
            }
        }
        evaluate(cx, &w);
        add_walk(cx, w);
    }
    // Not checked: a hook waiting, or a definition with no code yet. Walked once it has.
    cx.lazy.live.todo.extend(pending);
}

/// The definitions [`assume`]d that exist by now live: a class with its methods. Each is found by
/// its name where its `DefId` holds something else (`Assumed::defs`).
fn assumed_live(cx: &mut Ctx) {
    for (d, name) in std::mem::take(&mut cx.lazy.live.assumed) {
        let here = |c: &Ctx, d: DefId| constructs::def_name(c, d).as_deref() == Some(&name);
        let found = Some(d).filter(|d| (d.0 as usize) < cx.info.len() && here(cx, *d));
        let found = found.or_else(|| {
            let all = 0..cx.info.len() as u32;
            all.map(DefId).find(|&d| here(cx, d))
        });
        match found {
            Some(d) if cx.adt(d).is_some() => live_classes(cx, Some(d)),
            Some(d) => _ = mark(cx, d),
            None => cx.lazy.live.assumed.push((d, name)),
        }
    }
}

/// Is `d` the code of a non-std module?
fn program_code(cx: &Ctx, d: DefId) -> bool {
    match &cx.info[d.0 as usize] {
        DefInfo::Fn(f) => !cx.modules[f.module].is_std,
        DefInfo::Global(g) => !cx.modules[g.module].is_std,
        _ => false,
    }
}

/// What one definition of a non-std module, walked into `w` from `start` on, tells the hooks that
/// wait for what the program does. std's own code doesn't count here: it puts its records into
/// `any` values in most programs, and where lowering builds its text conversions and a float hook
/// stayed unchecked, it calls the runtime. The integer hooks, which have no such fallback, count
/// the constructs of live code, std's included.
fn note_program_code(
    cx: &mut Ctx,
    w: &Walk,
    (types, conversions, into_any, first_text): (usize, usize, usize, usize),
) {
    cx.lazy.live.texts.extend(&w.texts[first_text..]);
    if !cx.lazy.record_hooks {
        super::record_conversions::note_user_conversions(
            cx,
            &w.types[types..],
            &w.conversions[conversions..],
            &w.into_any[into_any..],
        );
    }
}

/// What `w` found is live.
fn add_walk(cx: &mut Ctx, w: Walk) {
    for &t in &w.conversions {
        if !cx.lazy.record_conversion {
            let holds = super::holds_string_record(cx, t, &mut FxHashSet::default());
            cx.lazy.record_conversion = holds;
        }
    }
    cx.lazy.live.kinds |= w.kinds;
    cx.lazy.live.ifaces.extend(w.ifaces);
    for d in w.refs {
        mark(cx, d);
    }
    let mut types = w.types;
    types.extend(w.formats.iter().chain(&w.texts));
    live_types(cx, types);
    let generators = [cx.generator_class(), cx.async_generator_class()];
    for (_, d) in (w.generators.iter().zip(generators)).filter(|(g, _)| **g) {
        live_classes(cx, d);
    }
    if w.catch_unwind {
        cx.lazy.live.present |= Construct::CatchUnwind.bit();
    }
    if w.async_host {
        let settled = hook_class(cx, hook_index::host_settled);
        live_classes(cx, settled);
    }
}

/// After the passes that make definitions once the bodies are checked (the implementations of
/// interfaces through `any`, the conversions of its functions, the adapters of `toString` and
/// `toJSON`), which lowering builds: their code is live. Those passes don't run again, and some
/// write what lowering reads, so what it reaches that wasn't checked yet isn't checked here: the
/// check runs again with it live from the start ([`too_late`]).
pub(crate) fn finish(cx: &mut Ctx) {
    if cx.lazy.modules.is_empty() {
        return;
    }
    cx.lazy.live.finishing = true;
    #[cfg(test)]
    tests::note_finish(cx);
    super::reach(cx);
}

/// The implementations and gated roots that the live classes, interfaces and kinds open, when
/// those changed since the last time.
fn open(cx: &mut Ctx) {
    if cx.lazy.live.kinds & DYNAMIC != 0 {
        dynamic_classes(cx);
    }
    let live = &cx.lazy.live;
    let now = (live.adts.len(), live.ifaces.len(), live.kinds);
    if live.opened == Some(now) {
        return;
    }
    cx.lazy.live.opened = Some(now);
    live_impls(cx);
    open_gates(cx);
}

/// The classes of the values lowering's glue makes from an `any` or finds in one, which no code
/// in the program has to mention: the `ArrayView` an `any` converted to an array is
/// (`velt_vir` `array_view.rs`), and the `Error`s it may hold, that its text and an uncaught
/// report of it call (`any_text`, `entry.rs` `push_dynamic_thrown`), with the `TypeError` its
/// operations throw among their subclasses.
fn dynamic_classes(cx: &mut Ctx) {
    if !std::mem::replace(&mut cx.lazy.live.dynamic_classes, true) {
        let view = hook_class(cx, hook_index::array_view);
        let error = cx.prelude_adt("Error");
        live_classes(cx, view);
        live_classes(cx, error);
    }
}

/// The class of hook `index` of the table, when the program has it.
fn hook_class(cx: &Ctx, index: usize) -> Option<DefId> {
    match cx.std_def(&HOOKS[index].qualified()) {
        Some(crate::ctx::Item::Def(d)) => Some(d),
        _ => None,
    }
}

/// The class `d`, when the program has it, as [`live_types`] records a type of it.
fn live_classes(cx: &mut Ctx, d: Option<DefId>) {
    if let Some(d) = d {
        live_types_of(cx, vec![], vec![d]);
    }
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

/// Make `d` live: whether it wasn't.
pub(super) fn mark(cx: &mut Ctx, d: DefId) -> bool {
    let new = cx.lazy.live.live.insert(d);
    if new {
        cx.lazy.live.todo.push(d);
    }
    new
}

/// Walk the checked code of live definition `d` into `w`; false if it is not checked yet.
fn walk_def(cx: &mut Ctx, d: DefId, w: &mut Walk) -> bool {
    let i = d.0 as usize;
    let ty = &cx.ty;
    if let DefInfo::Fn(f) = &cx.info[i] {
        w.async_host |= f.host.is_some() && f.is_async;
    }
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
            if f.is_generator {
                w.generators[f.is_async as usize] = true;
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
            if w.last_type != Some(t) {
                w.last_type = Some(t);
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
        E::ToDyn { expr, .. } => {
            w.formats.push(expr.ty);
            w.into_any.push((expr.ty, e.ty));
        }
        E::Throw(v) => w.formats.push(v.ty),
        E::Call {
            callee: Callee::Intrinsic(I::ToString | I::TemplateToString),
            args,
        } => w.texts.extend(args.iter().map(|a| a.ty)),
        E::Call {
            callee: Callee::Intrinsic(I::Print | I::PrintErr),
            args,
        } => {
            // A float is written by `numberWrite`, whose code holds the integer writer.
            w.prints_float |= (args.iter()).any(|a| matches!(ty.kind(a.ty), TyKind::Float(_)));
            w.formats.extend(args.iter().map(|a| a.ty));
        }
        E::Call {
            callee: Callee::Intrinsic(I::Inspect | I::ErrorText),
            args,
        } => w.formats.extend(args.iter().map(|a| a.ty)),
        E::Call {
            callee: Callee::Intrinsic(I::AnyTo | I::AnyToExact),
            ..
        } => w.conversions.push(e.ty),
        E::Call {
            callee: Callee::Intrinsic(I::CatchUnwind),
            ..
        } => w.catch_unwind = true,
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

/// Record the types `todo` and their parts as live: their constructs, and the methods of each
/// class or struct among them (with its base classes, subclasses and fields' types), and of the
/// implementations for them.
fn live_types(cx: &mut Ctx, todo: Vec<TyId>) {
    live_types_of(cx, todo, vec![]);
}

/// [`live_types`], and the classes and structs `adts`.
fn live_types_of(cx: &mut Ctx, mut todo: Vec<TyId>, mut adts: Vec<DefId>) {
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
                // The `format` slot of its table (`velt_vir` `glue/vtable.rs`). A class's is
                // left out of a program that formats nothing (`Cx::format_demanded`).
                TyKind::Dyn(..) => format_bit(cx, t),
                // A rejected promise not handled is reported with its error.
                TyKind::Promise(_, e) | TyKind::Result(_, e) => format_bit(cx, *e),
                _ => 0,
            };
            cx.lazy.live.present |= bit;
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
/// runs them). Whether std's or the program's, their text conversions count for the float hooks.
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
    cx.lazy.live.texts.extend(&w.texts);
    evaluate(cx, &w);
    add_walk(cx, w);
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

#[path = "live_constructs.rs"]
mod constructs;
#[path = "live_roots.rs"]
mod roots;
pub(super) use constructs::def_name;
use constructs::{demanded_for, evaluate, format_bit};
pub(crate) use constructs::{too_late, Assumed};
use roots::seed;

#[cfg(test)]
#[path = "live_tests.rs"]
mod tests;
