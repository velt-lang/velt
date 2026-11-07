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
//! ([`crate::dispatch`]), as `record_keys` carries key requirements. Where it can't tell, it
//! refuses and says what to write instead: a value reaches the methods of an `extend` that
//! implements an interface through `any` too, with no site at all, so such a method can't store
//! its type parameter ([`refused_extends`]), and a walk that reaches a limit reports it.
//! [`check_field_writes`] carries a write to a field of a type parameter the same way: a struct
//! given for that parameter would change, which its fields don't.

use rustc_hash::{FxHashMap, FxHashSet};
use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::dispatch::{add_work, instantiate, Pass, Rounds};
use crate::hir::{AdtKind, DefId, ImplDef, TyId, TyKind};
use crate::inst_sites::{carry_guard, resolve_param_calls, ParamCall, Uses};
use crate::types::{children, collect_params};

/// How deep [`stored_types`] follows a type, and how many struct instances it looks into per
/// function: a struct that refers to itself at a growing argument (`L<T>` holding an `L<T[]>`)
/// has no end. Past either, a type that mentions a parameter is refused ([`LIMIT`]).
const MAX_WALK_DEPTH: u32 = 32;
const MAX_WALK: usize = 4096;

/// The deepest type carried to a caller: one that grows at each call of a recursive function
/// (`f<TT<T>>` in `f<T>`) would never end. A deeper type is refused ([`LIMIT`]); a param call's
/// receiver that deep is dropped, as a primitive guard can end such a recursion legally.
const MAX_CARRIED_DEPTH: u32 = 16;

const LIMIT: &str = "this type is nested too deeply to tell whether a struct's field holds a tuple, which a struct can't hold yet";

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

/// One function's walk of the types it mentions.
#[derive(Default)]
struct Walk {
    seen: FxHashSet<TyId>,
    /// The struct instances looked into.
    structs: usize,
    /// A limit stopped the walk at a type that mentions a parameter.
    cut: bool,
}

/// The types `ty`, mentioned in a generic body, stores in a struct field and that only an
/// instantiation can tell from a tuple (see [`field_types`]), in any struct instance it holds.
fn stored_types(cx: &mut Ctx, ty: TyId, w: &mut Walk, out: &mut Vec<TyId>, depth: u32) {
    if !w.seen.insert(ty) {
        return;
    }
    if depth > MAX_WALK_DEPTH || w.structs >= MAX_WALK {
        w.cut |= is_generic(cx, ty);
        return;
    }
    let kind = cx.ty.kind(ty).clone();
    if let TyKind::Adt(d, args) = &kind {
        let value = cx.adt(*d).is_some_and(|a| {
            a.kind == AdtKind::Struct && a.decl.is_some_and(|decl| !decl.c_struct)
        }) && !cx.place_struct(*d);
        if value {
            w.structs += 1;
            let fields: Vec<TyId> = cx
                .adt(*d)
                .map(|a| a.fields.iter().map(|f| f.ty).collect())
                .unwrap_or_default();
            for f in fields {
                let s = cx.subst(f, args);
                field_types(cx, s, out, 0);
                stored_types(cx, s, w, out, depth + 1);
            }
        }
    }
    for c in children(&kind) {
        stored_types(cx, c, w, out, depth + 1);
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
    /// Those of `needs` a function's own body brings, not a method of a type it mentions.
    in_body: FxHashSet<(DefId, TyId)>,
    pneeds: FxHashMap<DefId, Vec<ParamCall>>,
    bad: Vec<(Span, TyId, Site)>,
    /// Where a limit stopped the check.
    limits: Vec<Span>,
    work: u64,
}

/// The types `f` takes from what `u` instantiates, and the param calls that reach it.
fn reached(cx: &mut Ctx, u: &Uses, s: &Found) -> (Vec<Reached>, Vec<(ParamCall, Span, Site)>) {
    let mut types = vec![];
    let mut pending: Vec<_> = (u.param_calls.iter())
        .map(|(p, s)| (p.clone(), *s, Site::Call))
        .collect();
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

/// Carries what `s` holds through `uses` until nothing changes (`refused`: methods not to read
/// through a param call, already reported; `bad`: is this type, given for what's carried, an
/// error).
fn carry(
    cx: &mut Ctx,
    uses: &[(DefId, Uses)],
    refused: &FxHashSet<DefId>,
    s: &mut Found,
    bad: fn(&mut Ctx, TyId) -> bool,
) {
    let reads = uses
        .iter()
        .enumerate()
        .flat_map(|(i, (_, u))| u.reads().map(move |d| (i, d)));
    let mut rounds = Rounds::new(uses.len(), reads);
    while let Some(i) = rounds.pop() {
        let (f, u) = &uses[i];
        s.work += u.size();
        let (mut types, pending) = reached(cx, u, s);
        let mut grew = false;
        for (p, span, site) in pending {
            let (needs, pneeds) = (&s.needs, &s.pneeds);
            let generic = resolve_param_calls(cx, &mut rounds, i, vec![p], &mut |cx, m, args| {
                if refused.contains(&m) {
                    return vec![];
                }
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
            if bad(cx, t) {
                s.bad.push((span, t, site));
                continue;
            }
            let mut parts = vec![];
            field_types(cx, t, &mut parts, 0);
            let n = s.needs.entry(*f).or_default();
            for part in parts {
                if !matches!(site, Site::Dispatched) {
                    s.in_body.insert((*f, part));
                }
                if !carried(cx, part) {
                    s.limits.push(span);
                } else if !n.contains(&(part, guard)) {
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

/// The implementations [`crate::dispatch`] doesn't list for a type: those of an `extend`
/// (`extend<T> Array<T> implements I`, `extend string implements I {}` with a default method).
/// A class's vtable and its own impls exist wherever the class does; these run where a value is
/// converted to the interface, or cast to it from `any`, of whatever type got there.
fn extended_impls(cx: &Ctx) -> Vec<u32> {
    let extended = |imp: &ImplDef| match *cx.ty.kind(imp.ty) {
        TyKind::Closure(_) => false,
        TyKind::Adt(..) => imp.methods.iter().any(|m| match &cx.info[m.0 as usize] {
            DefInfo::Fn(f) => f.owner.is_none(),
            _ => false,
        }),
        _ => true,
    };
    (0..cx.impls.len() as u32)
        .filter(|i| extended(&cx.impls[*i as usize]))
        .collect()
}

/// Which of `needs`, the types method `m` of the `extend` at `index` stores in struct fields
/// (in the method's own terms), can't be shown not to be a tuple: it is one for this `extend`,
/// or depends on the `extend`'s parameters. One that depends only on the method's own type
/// parameters is given at the call, and what the type the `extend` is on stores is what this
/// check follows for that type anyway: `this` in `extend<A> Pair<A, i64>` isn't a new way to
/// hold an `A`.
fn unprovable(cx: &mut Ctx, index: u32, m: DefId, needs: &[TyId]) -> Option<TyId> {
    let imp = &cx.impls[index as usize];
    let (ty, iface, generics, iface_args) =
        (imp.ty, imp.iface, imp.generics, imp.iface_args.clone());
    let default = cx
        .iface(iface)
        .is_some_and(|i| i.methods.iter().any(|d| d.default == Some(m)));
    let mut on_target = vec![];
    stored_types(cx, ty, &mut Walk::default(), &mut on_target, 0);
    needs.iter().copied().find(|t| {
        if !default {
            let mut ps = vec![];
            collect_params(&cx.ty, *t, &mut ps);
            return !on_target.contains(t) && ps.iter().any(|p| *p < generics);
        }
        // A default method's parameters are the interface's arguments and the implementor.
        let mut args = iface_args.clone();
        args.push(ty);
        let Some(t) = instantiate(cx, *t, &args) else {
            return false;
        };
        let mut parts = vec![];
        field_types(cx, t, &mut parts, 0);
        holds_tuple(cx, t, 0) || !parts.is_empty()
    })
}

/// `t`, a type in method `m`, as the program writes it: a parameter by its name.
fn written(cx: &Ctx, m: DefId, t: TyId) -> String {
    let name = match (cx.ty.kind(t), &cx.info[m.0 as usize]) {
        (TyKind::Param(i), DefInfo::Fn(f)) => f.generics.names.get(*i as usize),
        _ => None,
    };
    name.cloned().unwrap_or_else(|| cx.display(t))
}

/// The methods of `extend`s that implement an interface and store what can't be shown not to be
/// a tuple. Nothing says which types reach such a method: a value of any type may be cast to the
/// interface from `any`. Each is reported once, here, and carried no further.
fn refused_extends(cx: &mut Ctx, s: &Found) -> FxHashSet<DefId> {
    let mut refused = FxHashSet::default();
    let mut found = vec![];
    for index in extended_impls(cx) {
        for m in cx.impls[index as usize].methods.clone() {
            let Some(needs) = s.needs.get(&m) else {
                continue;
            };
            let own = needs.iter().map(|(t, _)| *t);
            let own: Vec<TyId> = own.filter(|t| s.in_body.contains(&(m, *t))).collect();
            if let Some(t) = unprovable(cx, index, m, &own) {
                if refused.insert(m) {
                    found.push((cx.def_spans[m.0 as usize], m, t));
                }
            }
        }
    }
    found.sort_by_key(|(s, _, _)| (s.file, s.lo));
    for (span, m, t) in found {
        let shown = written(cx, m, t);
        cx.error(
            Diagnostic::error(
                format!("this method of an `extend` stores `{shown}` in a struct's field, and it implements an interface, so nothing shows that this isn't a tuple, which a struct can't hold yet"),
                span,
            )
            .with_note("a value of any type reaches the method through the interface or `any`; take the interface off the `extend` and call the method directly, or make the struct a class, or give the field a type of its own"),
        );
    }
    refused
}

/// A tuple given for a type a generic body stores in a struct field, through any chain of
/// generic calls, closures and dispatched methods, is an error where it is given.
/// `uses`: the instantiation sites `record_keys` collected and resolved dynamic dispatch for.
pub(crate) fn check_generic_bodies(cx: &mut Ctx, uses: &[(DefId, Uses)]) {
    let mut s = Found::default();
    for (f, u) in uses {
        let (mut w, mut own) = (Walk::default(), vec![]);
        for (t, span) in &u.types {
            stored_types(cx, *t, &mut w, &mut own, 0);
            if std::mem::take(&mut w.cut) {
                s.limits.push(*span);
            }
        }
        if !own.is_empty() {
            own.sort_unstable_by_key(|t| t.0);
            own.dedup();
            s.in_body.extend(own.iter().map(|t| (*f, *t)));
            s.needs
                .insert(*f, own.into_iter().map(|t| (t, 0)).collect());
        }
    }
    if s.needs.is_empty() {
        report(cx, vec![], s.limits);
        return;
    }
    let (own, limits) = (s.needs.clone(), s.limits.clone());
    let none = FxHashSet::default();
    carry(cx, uses, &none, &mut s, is_tuple);
    let refused = refused_extends(cx, &s);
    if !refused.is_empty() {
        // Without them: what reached them through an interface value came from every instance
        // of the type interned so far, which needn't be anything in the program.
        let work = s.work;
        let in_body = std::mem::take(&mut s.in_body);
        s = Found {
            needs: own,
            in_body,
            limits,
            work,
            ..Found::default()
        };
        carry(cx, uses, &refused, &mut s, is_tuple);
    }
    add_work(Pass::StructTuples, s.work);
    report(cx, s.bad, s.limits);
}

fn is_tuple(cx: &mut Ctx, t: TyId) -> bool {
    holds_tuple(cx, t, 0)
}

/// A struct given for a type parameter a generic body writes a field of (`v.x = 1`, `v: T`, `T
/// extends { x: number }`): the write would change a struct, whose fields are readonly, and
/// reach the caller's value, as a struct argument may be passed by address.
pub(crate) fn check_field_writes(cx: &mut Ctx, uses: &[(DefId, Uses)]) {
    let writes = std::mem::take(&mut cx.param_field_writes);
    if writes.is_empty() {
        return;
    }
    let mut s = Found::default();
    for (f, t) in writes {
        let n = s.needs.entry(f).or_default();
        if !n.contains(&(t, 0)) {
            n.push((t, 0));
        }
    }
    let struct_given = |cx: &mut Ctx, t: TyId| cx.declared_struct(cx.ty.core(t).unwrap_or(t));
    carry(cx, uses, &FxHashSet::default(), &mut s, struct_given);
    add_work(Pass::StructTuples, s.work);
    s.bad.sort_by_key(|(s, _, _)| (s.file, s.lo));
    let mut reported: FxHashSet<Span> = FxHashSet::default();
    for (span, t, _) in s.bad {
        if reported.insert(span) {
            let shown = cx.display(t);
            cx.error(
                Diagnostic::error(
                    format!("the function called here writes a field of a struct (`{shown}`), and struct fields are readonly"),
                    span,
                )
                .with_note("a struct value never changes: return a new value from the function, or make the type a class"),
            );
        }
    }
}

/// Reports each place a tuple was given once, in source order.
fn report(cx: &mut Ctx, mut bad: Vec<(Span, TyId, Site)>, mut limits: Vec<Span>) {
    bad.sort_by_key(|(s, _, _)| (s.file, s.lo));
    let mut reported: FxHashSet<Span> = FxHashSet::default();
    limits.sort_by_key(|s| (s.file, s.lo));
    for span in limits {
        if reported.insert(span) {
            cx.error(
                Diagnostic::error(LIMIT, span).with_note(
                    "write the type with fewer layers of aliases or generics, or make the struct a class",
                ),
            );
        }
    }
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
