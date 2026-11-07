//! A declared `struct` (#641) can't hold a tuple yet, as a C struct can't (collect/c_struct.rs).
//! A struct is a value that copies inline and whose fields are read-only, but a tuple is stored
//! as lowering decides per tuple type (boxing/tuples.rs: counted once the program writes and
//! copies it), and its elements stay writable. A tuple field would make the struct's layout
//! depend on code elsewhere, let a write through `s.t[0]` reach every copy, and let a copy share
//! a counted tuple it doesn't count. Fixed-size arrays (`T[N]`, #1866 area 4) will hold such
//! elements inline. Like `void_fields`, this checks every instance the program's types mention,
//! after all bodies, so a generic struct is checked with the tuple it is instantiated with; a
//! struct holding another holds what that one holds, and the inner one is reported. An instance
//! made only inside a generic body (`G<T>` in `outer<T>`) is checked where the body is
//! instantiated: [`check_generic_bodies`] carries the types a generic function stores in struct
//! fields to every instantiation site ([`crate::inst_sites`]) and dispatched method
//! ([`crate::dispatch`]), as `record_keys` carries key requirements, and to where a value reaches
//! an `extend`'s methods ([`extended_sites`]), which those don't show.

use rustc_hash::{FxHashMap, FxHashSet};
use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::dispatch::{add_work, instantiate, Pass, Rounds};
use crate::hir::{AdtKind, DefId, TyId, TyKind};
use crate::inst_sites::{
    carry_guard, extended_impls, extended_sites, resolve_param_calls, ParamCall, Uses,
};
use crate::types::{children, collect_params};

/// How deep [`stored_types`] follows struct fields and what they mention, and how many types it
/// looks at per function: a type that refers to itself at a growing argument (`L<T>` holding an
/// `L<T[]>`) has no end, and that definition is reported on its own.
const MAX_WALK_DEPTH: u32 = 32;
const MAX_WALK: usize = 4096;

/// The deepest type carried to a caller. Only a type that grows at each call of a recursive
/// function (`f<TT<T>>` in `f<T>`) passes this, and without a limit carrying it wouldn't end;
/// `instantiation_cycles` reports that function, or a primitive guard ends its recursion.
const MAX_CARRIED_DEPTH: u32 = 16;

pub(crate) fn check_instantiations(cx: &mut Ctx) {
    let mut reported: FxHashSet<(DefId, String)> = FxHashSet::default();
    let n = cx.ty.table.len() as u32;
    for i in 0..n {
        let t = TyId(i);
        let TyKind::Adt(d, args) = cx.ty.kind(t).clone() else {
            continue;
        };
        let Some(a) = cx.adt(d) else { continue };
        let value = a.kind == AdtKind::Struct
            && a.decl.is_some_and(|decl| !decl.c_struct)
            && !cx.place_struct(d);
        if !value {
            continue;
        }
        let name = a
            .decl
            .map(|decl| decl.name.name.clone())
            .unwrap_or_default();
        let def_span = a.span;
        let fields: Vec<(String, TyId, Span)> = a
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty, f.span))
            .collect();
        for (field, fty, field_span) in fields {
            if cx.ty.has_error(fty) || reported.contains(&(d, field.clone())) {
                continue;
            }
            let ty = cx.subst(fty, &args);
            if !holds_tuple(cx, ty, 0) {
                continue;
            }
            reported.insert((d, field.clone()));
            let span = match field_span {
                Span::DUMMY => def_span,
                s => s,
            };
            let shown = cx.display(ty);
            let instance = match args.is_empty() {
                true => String::new(),
                false => format!(" (in `{}`)", cx.display(t)),
            };
            cx.error(
                Diagnostic::error(
                    format!("field `{field}` of struct `{name}` holds a tuple (`{shown}`){instance}, which a struct can't hold yet"),
                    span,
                )
                .with_note("a struct is a value copied inline with read-only fields, and a tuple's storage and elements are neither; until fixed-size arrays (`T[N]`) declare the elements as fields of their own, or make the type a class"),
            );
        }
    }
}

/// Is `ty` a tuple, or a tuple or null, or a union with a tuple member, or a deferred type that
/// reduces to one of those?
fn holds_tuple(cx: &mut Ctx, ty: TyId, depth: u32) -> bool {
    if depth > 8 {
        return false;
    }
    match cx.ty.kind(ty).clone() {
        TyKind::Tuple(_) => true,
        TyKind::Option(x, _) => holds_tuple(cx, x, depth + 1),
        TyKind::Op(..) if !is_generic(cx, ty) => {
            let r = cx.reduce_op(ty);
            r != ty && holds_tuple(cx, r, depth + 1)
        }
        _ => match cx.union_members(ty) {
            Some(ms) => ms.into_iter().any(|m| holds_tuple(cx, m, depth + 1)),
            None => false,
        },
    }
}

fn is_generic(cx: &Ctx, ty: TyId) -> bool {
    let mut ps = vec![];
    collect_params(&cx.ty, ty, &mut ps);
    !ps.is_empty()
}

/// The types `ty`, mentioned in a generic body, stores in a struct field and that only an
/// instantiation can tell from a tuple (see [`field_types`]), in any struct instance it holds.
fn stored_types(
    cx: &mut Ctx,
    ty: TyId,
    seen: &mut FxHashSet<TyId>,
    out: &mut Vec<TyId>,
    depth: u32,
) {
    if depth > MAX_WALK_DEPTH || seen.len() >= MAX_WALK || !seen.insert(ty) {
        return;
    }
    let kind = cx.ty.kind(ty).clone();
    if let TyKind::Adt(d, args) = &kind {
        let value = cx.adt(*d).is_some_and(|a| {
            a.kind == AdtKind::Struct && a.decl.is_some_and(|decl| !decl.c_struct)
        }) && !cx.place_struct(*d);
        if value {
            let fields: Vec<TyId> = cx
                .adt(*d)
                .map(|a| a.fields.iter().map(|f| f.ty).collect())
                .unwrap_or_default();
            for f in fields {
                let s = cx.subst(f, args);
                field_types(cx, s, out, 0);
                stored_types(cx, s, seen, out, depth + 1);
            }
        }
    }
    for c in children(&kind) {
        stored_types(cx, c, seen, out, depth + 1);
    }
}

/// The parts of a field of type `ty` that an instantiation can make a tuple, the way
/// [`holds_tuple`] looks through it: a parameter or a deferred type, as itself, or null, or a
/// union member.
fn field_types(cx: &mut Ctx, ty: TyId, out: &mut Vec<TyId>, depth: u32) {
    if depth > 8 {
        return;
    }
    match cx.ty.kind(ty).clone() {
        TyKind::Param(_) => out.push(ty),
        TyKind::Op(..) if is_generic(cx, ty) => out.push(ty),
        TyKind::Option(x, _) => field_types(cx, x, out, depth + 1),
        _ => {
            if let Some(ms) = cx.union_members(ty) {
                for m in ms {
                    field_types(cx, m, out, depth + 1);
                }
            }
        }
    }
}

/// How deeply `t` nests, counted up to [`MAX_CARRIED_DEPTH`] and one past it.
fn nesting(cx: &Ctx, t: TyId, depth: u32) -> u32 {
    if depth > MAX_CARRIED_DEPTH {
        return depth;
    }
    let parts = children(cx.ty.kind(t));
    let deepest = parts.into_iter().map(|c| nesting(cx, c, depth + 1)).max();
    deepest.unwrap_or(depth)
}

fn carried(cx: &Ctx, t: TyId) -> bool {
    nesting(cx, t, 0) <= MAX_CARRIED_DEPTH
}

/// How a generic function was reached at the span an error is reported at.
#[derive(Clone, Copy)]
enum Site {
    Call,
    /// A method that runs for a type mentioned there (`Uses::dyn_calls`).
    Dispatched,
}

/// A type a function stores in a struct field, and its guard bits (`inst_sites::carry_guard`).
type Need = (TyId, u64);

/// A type reaching a function, with where to report it and how it got there.
type Reached = (Need, Span, Site);

/// What is known so far: the types each function stores in struct fields, in terms of its own
/// parameters; the param calls whose receiver is generic in each function; the tuples found.
#[derive(Default)]
struct Found {
    needs: FxHashMap<DefId, Vec<Need>>,
    pneeds: FxHashMap<DefId, Vec<ParamCall>>,
    bad: Vec<(Span, TyId, Site)>,
    work: u64,
}

/// The types `f` takes from what `u` instantiates, and the param calls that reach it (`extra`:
/// those [`extended_sites`] found).
fn reached(
    cx: &mut Ctx,
    u: &Uses,
    extra: &[(ParamCall, Span)],
    s: &Found,
) -> (Vec<Reached>, Vec<(ParamCall, Span, Site)>) {
    let mut types = vec![];
    let sites = u.param_calls.iter().chain(extra);
    let mut pending: Vec<_> = sites.map(|(p, s)| (p.clone(), *s, Site::Call)).collect();
    for ((g, targs, span), site) in u.calls.iter().zip(&u.guards) {
        for (t, need) in s.needs.get(g).into_iter().flatten() {
            if let Some(guard) = carry_guard(cx, *need, targs, *site) {
                types.push(((cx.subst(*t, targs), guard), *span, Site::Call));
            }
        }
        for p in s.pneeds.get(g).into_iter().flatten() {
            pending.push((p.subst(cx, targs), *span, Site::Call));
        }
    }
    for c in &u.closures {
        let span = cx.def_spans[c.0 as usize];
        for need in s.needs.get(c).into_iter().flatten() {
            types.push((*need, span, Site::Call));
        }
        for p in s.pneeds.get(c).into_iter().flatten() {
            pending.push((p.clone(), span, Site::Call));
        }
    }
    for (m, args, span) in &u.dyn_calls {
        for (t, need) in s.needs.get(m).into_iter().flatten() {
            let Some(guard) = carry_guard(cx, *need, args, 0) else {
                continue;
            };
            if let Some(t) = instantiate(cx, *t, args) {
                types.push(((t, guard), *span, Site::Dispatched));
            }
        }
        for p in s.pneeds.get(m).into_iter().flatten() {
            let p = p.instantiate(cx, args);
            pending.extend(p.map(|p| (p, *span, Site::Dispatched)));
        }
    }
    (types, pending)
}

/// Carries what `s` holds through `uses` until nothing changes.
fn carry(
    cx: &mut Ctx,
    uses: &[(DefId, Uses)],
    extra: &FxHashMap<DefId, Vec<(ParamCall, Span)>>,
    s: &mut Found,
) {
    let reads = uses
        .iter()
        .enumerate()
        .flat_map(|(i, (_, u))| u.reads().map(move |d| (i, d)));
    let mut rounds = Rounds::new(uses.len(), reads);
    while let Some(i) = rounds.pop() {
        let (f, u) = &uses[i];
        s.work += u.size();
        let more = extra.get(f).map_or(&[][..], |v| &v[..]);
        let (mut types, pending) = reached(cx, u, more, s);
        let mut grew = false;
        for (p, span, site) in pending {
            let (needs, pneeds) = (&s.needs, &s.pneeds);
            let generic = resolve_param_calls(cx, &mut rounds, i, vec![p], &mut |cx, m, args| {
                for (t, _) in needs.get(&m).into_iter().flatten() {
                    types.push(((cx.subst(*t, args), 0), span, site));
                }
                let ps = pneeds.get(&m).into_iter().flatten();
                ps.map(|p| p.subst(cx, args)).collect()
            });
            for p in generic {
                let parts = std::iter::once(&p.recv).chain(&p.margs);
                if parts.into_iter().any(|t| !carried(cx, *t)) {
                    continue;
                }
                let n = s.pneeds.entry(*f).or_default();
                if !n.contains(&p) {
                    n.push(p);
                    grew = true;
                }
            }
        }
        for ((t, guard), span, site) in types {
            if holds_tuple(cx, t, 0) {
                s.bad.push((span, t, site));
                continue;
            }
            let mut parts = vec![];
            field_types(cx, t, &mut parts, 0);
            parts.retain(|p| carried(cx, *p));
            let n = s.needs.entry(*f).or_default();
            for part in parts {
                if !n.contains(&(part, guard)) {
                    n.push((part, guard));
                    grew = true;
                }
            }
        }
        if grew {
            rounds.changed(*f);
        }
    }
}

/// A tuple given for a type a generic body stores in a struct field, through any chain of
/// generic calls, closures and dispatched methods, is an error where it is given.
/// `uses`: the instantiation sites `record_keys` collected and resolved dynamic dispatch for.
pub(crate) fn check_generic_bodies(cx: &mut Ctx, uses: &[(DefId, Uses)]) {
    let mut s = Found::default();
    for (f, u) in uses {
        let (mut seen, mut own) = (FxHashSet::default(), vec![]);
        for (t, _) in &u.types {
            stored_types(cx, *t, &mut seen, &mut own, 0);
        }
        if !own.is_empty() {
            own.sort_unstable_by_key(|t| t.0);
            own.dedup();
            s.needs
                .insert(*f, own.into_iter().map(|t| (t, 0)).collect());
        }
    }
    if s.needs.is_empty() {
        return;
    }
    carry(cx, uses, &FxHashMap::default(), &mut s);
    // An `extend`'s methods are looked for only if one of them stores something: the search
    // reads every body, and few programs need it.
    let impls = extended_impls(cx);
    let methods = impls.iter().flat_map(|i| &cx.impls[*i as usize].methods);
    if methods
        .into_iter()
        .any(|m| s.needs.get(m).is_some_and(|n| !n.is_empty()) || s.pneeds.contains_key(m))
    {
        let sites = extended_sites(cx, &impls);
        carry(cx, uses, &sites, &mut s);
    }
    add_work(Pass::StructTuples, s.work);
    report(cx, s.bad);
}

/// Reports each place a tuple was given once, in source order.
fn report(cx: &mut Ctx, mut bad: Vec<(Span, TyId, Site)>) {
    bad.sort_by_key(|(s, _, _)| (s.file, s.lo));
    let mut reported: FxHashSet<Span> = FxHashSet::default();
    for (span, t, site) in bad {
        if !reported.insert(span) {
            continue;
        }
        let shown = cx.display(t);
        let what = match site {
            Site::Call => "this call stores",
            Site::Dispatched => "a method of this type stores",
        };
        cx.error(
            Diagnostic::error(
                format!("{what} a tuple (`{shown}`) in a struct's field, which a struct can't hold yet"),
                span,
            )
            .with_note("the function builds a struct whose field has this type argument's type; until fixed-size arrays (`T[N]`) pass the elements as fields of a struct of their own, or use a class"),
        );
    }
}
