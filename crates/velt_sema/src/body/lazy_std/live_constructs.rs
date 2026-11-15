//! The constructs of `hook_demand::Construct` in what walking live code found ([`super`]).

use velt_common::std_hooks::{Construct, ConstructSet};

use super::Walk;
use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{self, DefId, TyId, TyKind};

/// The constructs of `w`'s conversions and formatted values.
pub(super) fn evaluate(cx: &mut Ctx, w: &Walk) {
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
    cx.lazy.live.prints_float |= w.prints_float;
}

/// What a round starts with, for what the round before learned too late to act on: what only
/// the passes after the bodies' check tell ([`too_late`], [`super::finish`]).
#[derive(Clone, Default)]
pub(crate) struct Assumed {
    pub(in super::super) constructs: ConstructSet,
    /// Live from the start: classes of values reported when thrown and not caught, and bodies
    /// that the code those passes make reaches, by `DefId` and name. Each is a collected
    /// definition, but in a module added during the check it may have another `DefId` next
    /// round: closures of bodies checked before the module was added take numbers too.
    pub(in super::super) defs: Vec<(DefId, String)>,
    /// A conversion to a record that those passes made ([`super::super::RECORD_HOOKS`]).
    pub(in super::super) record_conversion: bool,
    /// Code running on the executor that those passes reached ([`super::live::Live::tasks`]).
    pub(in super::super) tasks: bool,
    /// Hooks (`HOOKS` indexes) that a `hookCall` reached by those passes calls: their modules are
    /// demanded when the call is checked ([`super::hook_stubs`]).
    pub(in super::super) hooks: Vec<usize>,
}

/// What lowering reports with the inferred `throws` of a live function, when thrown and not caught
/// and not yet accounted for, added to `assumed`: `Construct::Format`, and the classes of those
/// values, with whatever takes one (`uncaughtHeader`). [`super::live`] sees what code throws, but
/// inference also gives a function what a call of a function value throws, and what the interface
/// or the base class it implements declares (`throws::infer`, the method groups), whatever its
/// body does. That is known only after the passes, too late to load a hook or to check a body, so
/// the check runs again with it assumed (`check_program`), and with what [`super::finish`] found.
/// A type parameter stands for what its callers' `throws` show. A module constant's initializer
/// has no caller, and reports what it runs: any type argument in it may be what a generic `throws`
/// stands for, by whatever expression the instance is reached. Whether anything was added.
pub(crate) fn too_late(cx: &mut Ctx, assumed: &mut Assumed) -> bool {
    if cx.lazy.modules.is_empty() {
        return false;
    }
    let late = std::mem::take(&mut cx.lazy.live.late);
    let new_defs: Vec<_> = (late.defs.into_iter())
        .filter(|(_, name)| !assumed.defs.iter().any(|(_, n)| n == name))
        .collect();
    // One assumed already and late again stays a stub, which lowering refuses: no further round.
    let mut added = !new_defs.is_empty()
        || assumed.constructs | late.constructs != assumed.constructs
        || late.record_conversion && !assumed.record_conversion
        || late.tasks && !assumed.tasks;
    assumed.constructs |= late.constructs;
    assumed.record_conversion |= late.record_conversion;
    assumed.tasks |= late.tasks;
    assumed.defs.extend(new_defs);
    for h in late.hooks {
        if !assumed.hooks.contains(&h) {
            assumed.hooks.push(h);
            added = true;
        }
    }
    let live: Vec<_> = cx.lazy.live.live.iter().copied().collect();
    let mut thrown: Vec<TyId> = (live.iter())
        .filter_map(|d| match &cx.info[d.0 as usize] {
            DefInfo::Fn(f) => f.throws,
            _ => None,
        })
        .collect();
    if thrown.iter().any(|&t| is_param(cx, t)) {
        for d in live {
            let ty = &cx.ty;
            if let DefInfo::Global(g) = &mut cx.info[d.0 as usize] {
                if let Some(e) = &mut g.init {
                    crate::visit::exprs_in(e, &mut |e| type_args_in(ty, e, &mut thrown));
                }
            }
        }
    }
    let thrown: Vec<TyId> = thrown.into_iter().filter(|&t| !is_param(cx, t)).collect();
    let bit = Construct::Format.bit();
    let format = !demanded_for(cx.lazy.live.present) & bit != 0
        && thrown.iter().any(|&t| format_bit(cx, t) != 0);
    if format {
        assumed.constructs |= bit;
    }
    let (mut classes, mut seen) = (vec![], rustc_hash::FxHashSet::default());
    let mut todo = thrown;
    while let Some(t) = todo.pop() {
        if seen.insert(t) {
            let kind = cx.ty.kind(t);
            match kind {
                // Not an object type's or a body's: those have no stable `DefId`, and no
                // deferred methods.
                TyKind::Adt(d, _)
                    if !cx.lazy.live.adts.contains(d)
                        && !assumed.defs.iter().any(|(a, _)| a == d)
                        && cx.collected(*d) =>
                {
                    classes.push(*d)
                }
                _ => {}
            }
            todo.extend(crate::types::children(kind));
        }
    }
    classes.sort();
    classes.dedup();
    added |= format || !classes.is_empty();
    let named = classes
        .into_iter()
        .filter_map(|d| Some((d, def_name(cx, d)?)));
    assumed.defs.extend(named);
    added
}

/// The hooks that print objects which hold themselves (std/lang/hooks/inspect_cycles.velt),
/// demanded with the other shapes but checked only once a live type can hold itself
/// ([`may_cycle`]). Lowering tracks no cycle where they are unchecked (they have a fallback).
const CYCLE_HOOKS: [usize; 8] = {
    use velt_common::std_hooks::hook_index as h;
    [
        h::open_object,
        h::inspect_enter,
        h::inspect_leave,
        h::inspect_circular,
        h::inspect_save,
        h::inspect_restore,
        h::inspect_begin,
        h::inspect_end,
    ]
};

/// Is the hook named `name` one that [`demand_gated`] marks, not the constructs alone? Besides
/// [`CYCLE_HOOKS`], the hook that prints an array (std/lang/hooks/inspect.velt `inspectArray`),
/// checked only once an array type is live: its `E[]` parameter makes the `Array` class live,
/// which a program without arrays doesn't check. Lowering writes an array with its own glue
/// where it is unchecked.
pub(super) fn gated(name: &str) -> bool {
    use velt_common::std_hooks::{hook_index, HOOKS};
    name == HOOKS[hook_index::inspect_array].name
        || CYCLE_HOOKS.iter().any(|&i| HOOKS[i].name == name)
}

/// Marks the [`gated`] hooks once their constructs are present and what they print is live: an
/// array type for the array's, a type that can hold itself for the cycle hooks. Whether any was
/// newly marked. A type that can hold itself found only after the passes is left to the next
/// round (`Assumed::hooks`).
pub(super) fn demand_gated(cx: &mut Ctx) -> bool {
    use velt_common::std_hooks::{hook_index, HookSet};
    let demanded = HookSet::demanded_by(demanded_for(cx.lazy.live.present));
    let mut marked = false;
    let arrays = cx.lazy.live.kinds & super::kind_bit(&TyKind::Array(TyId(0))) != 0;
    if arrays && demanded.contains(hook_index::inspect_array) && !cx.lazy.live.finishing {
        marked |= mark_hook(cx, hook_index::inspect_array);
    }
    if cx.lazy.cycles || !demanded.contains(hook_index::inspect_enter) {
        return marked;
    }
    let assumed = cx.lazy.stubs.assumed(hook_index::inspect_enter);
    if !assumed && !may_cycle(cx) {
        return marked;
    }
    cx.lazy.cycles = true;
    if cx.lazy.live.finishing {
        cx.lazy.live.late.hooks.extend(CYCLE_HOOKS);
        return marked;
    }
    for i in CYCLE_HOOKS {
        marked |= mark_hook(cx, i);
    }
    marked
}

/// Notes what the program's own code puts into an `any` (`into_any`, `(value, any)` types): a
/// value of a type parameter makes an `any` able to hold anything ([`may_cycle`]).
pub(super) fn note_program_dyn(cx: &mut Ctx, into_any: &[(TyId, TyId)]) {
    for &(t, _) in into_any {
        cx.ty.visit(t, &mut |k| {
            cx.lazy.dyn_generic |= matches!(k, TyKind::Param(_))
        });
    }
}

/// Demands hook `index`'s module and marks the hook live: whether it wasn't.
fn mark_hook(cx: &mut Ctx, index: usize) -> bool {
    let h = &velt_common::std_hooks::HOOKS[index];
    cx.demand_std(&h.module());
    match cx.std_def(&h.qualified()) {
        Some(crate::ctx::Item::Def(d)) => super::mark(cx, d),
        _ => false,
    }
}

/// How deep [`reaches_back`] looks before it answers yes (a generic type can expand to a new
/// type at every level).
const CYCLE_DEPTH: u32 = 32;

/// Can a value of a live type hold itself, so that printing it must track cycles (`<ref *1>`,
/// `[Circular *1]`)? Yes when a live type reaches itself through what it holds (fields, elements,
/// entries, union members, for a class each live subclass, and for an `any` or an interface value
/// what live code puts into one). A type parameter holds what its instances hold, and those are
/// live types of their own; a generic subclass met through its base is each of its live
/// instantiations, or anything where generic code has one with its own parameters (lowering makes
/// it per instance, which sema doesn't see). The types found closed are remembered while the live
/// classes, what goes into an `any` and the live instantiations of generic subclasses don't
/// change.
fn may_cycle(cx: &mut Ctx) -> bool {
    let mut types: Vec<TyId> = cx.lazy.live.types.iter().copied().collect();
    let mut insts = Insts::default();
    for &t in &types {
        if let TyKind::Adt(d, args) = cx.ty.kind(t) {
            if !args.is_empty() && cx.adt(*d).is_some_and(|a| a.base.is_some()) {
                let mut open = false;
                cx.ty.visit(t, &mut |k| {
                    open |= matches!(k, TyKind::Param(_) | TyKind::Op(..))
                });
                let e = insts.entry(*d).or_default();
                e.0.push(t);
                e.1 |= open;
            }
        }
    }
    let seen = (
        cx.lazy.live.adts.len(),
        cx.lazy.dyn_held.len(),
        cx.lazy.dyn_generic,
        insts.values().map(|e| e.0.len()).sum(),
    );
    if cx.lazy.cycle_seen != seen {
        cx.lazy.cycle_closed.clear();
        cx.lazy.cycle_seen = seen;
    }
    types.retain(|&t| {
        !cx.lazy.cycle_closed.contains(&t) && !matches!(cx.ty.kind(t), TyKind::Dyn(..))
    });
    types.sort_unstable_by_key(|t| t.0);
    for v in insts.values_mut() {
        v.0.sort_unstable_by_key(|t| t.0);
    }
    let mut closed = std::mem::take(&mut cx.lazy.cycle_closed);
    let mut path = Path::default();
    // An `any` or interface value at the top holds a value of some live type, checked as such.
    let found = (types.into_iter())
        .any(|t| reaches_back(cx, t, (&mut path, &insts), &mut closed, (0, 0)).0);
    cx.lazy.cycle_closed = closed;
    found
}

/// The live instantiations of each generic subclass (a class with a base and type parameters), and
/// whether one has a type parameter in it.
type Insts = rustc_hash::FxHashMap<DefId, (Vec<TyId>, bool)>;

/// The walk of [`may_cycle`]: the types on the way from a live type, each with how many objects
/// lowering tracks (class and struct instances, `Map`s and `Set`s) come before it.
type Path = rustc_hash::FxHashMap<TyId, u32>;

/// Does `t` (on the way `path`, after `tracked` tracked objects) reach a type on `path` with a
/// tracked object on the cycle? A cycle of arrays and `any`s alone is not tracked (lowering checks
/// no array for one). Also whether the walk from `t` met the way back to a type on `path` at all:
/// only a type whose walk met none is closed whatever the way to it.
fn reaches_back(
    cx: &mut Ctx,
    t: TyId,
    (path, insts): (&mut Path, &Insts),
    closed: &mut rustc_hash::FxHashSet<TyId>,
    (depth, tracked): (u32, u32),
) -> (bool, bool) {
    if closed.contains(&t) {
        return (false, false);
    }
    if depth > CYCLE_DEPTH {
        return (true, true);
    }
    if let Some(&before) = path.get(&t) {
        return (tracked > before, true);
    }
    path.insert(t, tracked);
    let inner = tracked + is_tracked(cx, t) as u32;
    let (mut found, mut back) = (false, false);
    match held(cx, t, insts) {
        Some(parts) => {
            for p in parts {
                let (f, b) = reaches_back(cx, p, (path, insts), closed, (depth + 1, inner));
                back |= b;
                if f {
                    found = true;
                    break;
                }
            }
        }
        None => found = true,
    }
    path.remove(&t);
    if !found && !back {
        closed.insert(t);
    }
    (found, back)
}

/// Does lowering track a value of type `t` for cycles (`format_once`): a class or struct
/// instance, a `Map` or a `Set`?
fn is_tracked(cx: &Ctx, t: TyId) -> bool {
    match cx.ty.kind(t) {
        TyKind::Map(..) => true,
        TyKind::Adt(d, _) => cx.adt(*d).is_some(),
        _ => false,
    }
}

/// The types a value of type `t` holds and printing shows; `None` when it can hold anything.
/// `insts`: the live instantiations of the generic subclasses.
fn held(cx: &mut Ctx, t: TyId, insts: &Insts) -> Option<Vec<TyId>> {
    let kind = cx.ty.kind(t).clone();
    Some(match kind {
        // What live code puts into one; anything when the program's generic code puts a type
        // parameter's value into one (its instances are not followed there). std's generic code
        // keeps the `any`s it makes to itself.
        TyKind::Dyn(..) if cx.lazy.dyn_generic => return None,
        TyKind::Dyn(..) => cx.lazy.dyn_held.iter().copied().collect(),
        TyKind::Array(e) | TyKind::Option(e, _) | TyKind::Shared(e) => vec![e],
        TyKind::Tuple(es) => es,
        TyKind::Map(a, b) | TyKind::Result(a, b) | TyKind::Promise(a, b) => vec![a, b],
        TyKind::Adt(d, args) => {
            if let Some(members) = cx.union_members(t) {
                return Some(members);
            }
            if let Some(e) = cx.enum_info(d) {
                let payloads: Vec<TyId> = (e.variants.iter())
                    .flat_map(|v| v.payload.iter().copied())
                    .collect();
                return Some(payloads.into_iter().map(|p| cx.subst(p, &args)).collect());
            }
            let Some(a) = cx.adt(d) else {
                return Some(vec![]);
            };
            let fields: Vec<TyId> = a.fields.iter().map(|f| f.ty).collect();
            let mut parts: Vec<TyId> = fields.into_iter().map(|f| cx.subst(f, &args)).collect();
            let subclasses = cx.lazy.subclasses.get(&d).cloned().unwrap_or_default();
            for s in subclasses
                .into_iter()
                .filter(|s| cx.lazy.live.adts.contains(s))
            {
                // A generic subclass as each of its live instantiations: its type arguments may
                // be how a value reaches itself. Anything when generic code makes one.
                match (cx.adt(s), insts.get(&s)) {
                    (Some(sa), _) if sa.generics.names.is_empty() => {
                        parts.push(cx.ty.intern(TyKind::Adt(s, vec![])))
                    }
                    (Some(_), Some((_, true))) => return None,
                    (Some(_), Some((ts, false))) => parts.extend(ts),
                    _ => {}
                }
            }
            parts
        }
        // Scalars, functions (printed by name) and type parameters (their instances are live
        // types of their own).
        _ => vec![],
    })
}

/// The constructs `present` demands the hooks of. The integer hooks act as one bit: a construct
/// present demands them all, so that glue, which lowering makes per type, doesn't depend on each
/// of its call sites being listed for the right construct (velt_common `hook_demand.rs`).
/// `CatchUnwind` is its own bit: it demands std's panic path, which only a program that catches
/// needs (and a source generator must not reach).
pub(super) fn demanded_for(present: ConstructSet) -> ConstructSet {
    let catch = Construct::CatchUnwind.bit();
    let integer = match present & !catch {
        0 => 0,
        _ => Construct::ALL.iter().fold(0, |set, c| set | c.bit()) & !catch,
    };
    integer | (present & catch)
}

/// The name that finds function, class or constant `d` again next round.
pub(in super::super) fn def_name(cx: &Ctx, d: DefId) -> Option<String> {
    match &cx.info[d.0 as usize] {
        DefInfo::Fn(f) => Some(f.name.clone()),
        DefInfo::Adt(a) => Some(a.qual_name.clone()),
        DefInfo::Global(g) => Some(g.name.clone()),
        _ => None,
    }
}

/// The type arguments `e` names or has in its type.
fn type_args_in(ty: &crate::types::Types, e: &hir::Expr, out: &mut Vec<TyId>) {
    if let TyKind::Adt(_, args) = ty.kind(e.ty) {
        out.extend(args);
    }
    match &e.kind {
        hir::ExprKind::Call {
            callee: hir::Callee::Def(_, targs),
            ..
        } => out.extend(targs),
        hir::ExprKind::New { type_args, .. } => out.extend(type_args),
        _ => {}
    }
}

fn is_param(cx: &Ctx, t: TyId) -> bool {
    matches!(cx.ty.kind(t), TyKind::Param(_) | TyKind::Op(..))
}

pub(super) fn format_bit(cx: &mut Ctx, t: TyId) -> ConstructSet {
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
