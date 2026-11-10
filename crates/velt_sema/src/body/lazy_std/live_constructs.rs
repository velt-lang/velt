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
        || late.record_conversion && !assumed.record_conversion;
    assumed.constructs |= late.constructs;
    assumed.record_conversion |= late.record_conversion;
    assumed.defs.extend(new_defs);
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
