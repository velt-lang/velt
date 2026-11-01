//! Generic types that refer to themselves at a growing type argument
//! (`type L<T> = { v: T; n: L<T[]> | null }`). TypeScript accepts them, but Velt lays out,
//! drops, compares and generates code for each instantiation of a type separately, and
//! `L<number>` needs `L<number[]>`, which needs `L<number[][]>`, without end: lowering never
//! finished (its stack overflowed), whether the program only held such a value or gave it to
//! `JSON.parse`, `JSON.stringify` or `deepEqual`.
//!
//! The check is the expansion test of [`crate::instantiation_cycles`] applied to type
//! definitions: a node per type parameter of a generic type, and for each type `G<…>` a member of
//! a definition `D` mentions (a field, a union member or enum payload, the base class), an edge
//! from every parameter of `D` to the parameter of `G` whose argument mentions it. The edge grows
//! when that argument is more than the bare parameter. A definition with a growing edge inside a
//! strongly connected component is *expansive*: every instantiation of it has infinitely many
//! types, and so has every type whose members reach one.
//!
//! Each function that mentions such a type (in an expression's type, a call's type arguments or
//! its signature) gets one error, at a call that instantiates a generic function with it when it
//! has one (`JSON.parse<L<number>>(…)`), else at its first mention. A program without generic
//! types that grow is not visited further.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, TyId, TyKind};
use crate::instantiation_cycles::Graph;
use crate::types::{children, collect_params};
use crate::visit;

/// An expansive definition: its type with its own parameters (`L<T>`) and the growing mention
/// that makes it so (`L<T[]>`), as messages show them.
struct Expansive {
    own: String,
    grown: String,
}

/// Reports every function that mentions a type of an expansive definition (`true`: something
/// was reported; the passes that follow must not run then).
pub(crate) fn check(cx: &mut Ctx) -> bool {
    let members = type_members(cx);
    let expansive = expansive_defs(cx, &members);
    if expansive.is_empty() {
        return false;
    }
    // The definitions whose types reach an expansive one, each with the one it reaches.
    let mut bad: FxHashMap<DefId, DefId> = expansive.keys().map(|d| (*d, *d)).collect();
    loop {
        let mut grew = false;
        for (d, tys) in &members {
            if bad.contains_key(d) {
                continue;
            }
            if let Some(root) = tys.iter().find_map(|t| reaches(cx, *t, &bad)) {
                bad.insert(*d, root);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    // The methods of an expansive type itself (and the closures in them) run only on its
    // values, which are reported where they are made.
    let own_methods: FxHashSet<String> = (cx.info.iter())
        .filter_map(|i| match i {
            DefInfo::Fn(f) if f.owner.is_some_and(|o| bad.contains_key(&o)) => Some(f.name.clone()),
            _ => None,
        })
        .collect();
    let mut reported = false;
    for i in 0..cx.defs.len() {
        let d = DefId(i as u32);
        match &cx.info[i] {
            DefInfo::Fn(f) => {
                let enclosing = f.name.split("::{closure").next().unwrap_or(&f.name);
                if own_methods.contains(enclosing) {
                    continue;
                }
            }
            // A module constant or variable of such a type.
            DefInfo::Global(g) => {
                let (ty, span) = (g.ty, g.span);
                if reaches(cx, ty, &bad).is_some() {
                    let shown = cx.display(ty);
                    report(cx, &expansive, &bad, span, (ty, shown), None);
                    reported = true;
                }
                continue;
            }
            _ => continue,
        }
        if let Some((span, ty, call)) = first_mention(cx, d, &bad) {
            // A generic function's own type parameters, by their names.
            let names = match &cx.info[i] {
                DefInfo::Fn(f) => f.generics.names.clone(),
                _ => vec![],
            };
            let shown = cx.display_in(ty, &names);
            report(cx, &expansive, &bad, span, (ty, shown), call);
            reported = true;
        }
    }
    reported
}

/// The member types of every struct, class, object type, union and enum (with its own type
/// parameters as `Param`).
fn type_members(cx: &Ctx) -> Vec<(DefId, Vec<TyId>)> {
    let mut out = vec![];
    for (i, info) in cx.info.iter().enumerate() {
        let tys: Vec<TyId> = match info {
            DefInfo::Adt(a) => (a.fields.iter().map(|f| f.ty)).chain(a.base).collect(),
            DefInfo::Enum(e) => (e.variants.iter())
                .flat_map(|v| v.payload.iter().copied())
                .collect(),
            DefInfo::Iface(f) => f.fields.iter().map(|f| f.ty).collect(),
            _ => continue,
        };
        out.push((DefId(i as u32), tys));
    }
    out
}

fn generic_names<'a>(cx: &'a Ctx, d: DefId) -> &'a [String] {
    match &cx.info[d.0 as usize] {
        DefInfo::Adt(a) => &a.generics.names,
        DefInfo::Enum(e) => &e.generics.names,
        DefInfo::Iface(f) => &f.generics.names,
        _ => &[],
    }
}

/// Every type `G<args>` (an object type, union or interface) in `t`, outermost first.
fn named_in(cx: &Ctx, t: TyId, f: &mut impl FnMut(DefId, &[TyId], TyId)) {
    let k = cx.ty.kind(t);
    if let TyKind::Adt(g, args) | TyKind::Dyn(g, args) = k {
        f(*g, args, t);
    }
    for c in children(k) {
        named_in(cx, c, f);
    }
}

/// An edge from a type parameter of a definition to one of a type its members mention:
/// `(from, to, grows, the mention)`.
type TypeEdge = ((DefId, u32), (DefId, u32), bool, TyId);

/// The expansive definitions, with what makes each so: every definition with a parameter in a
/// strongly connected component that has a growing edge.
fn expansive_defs(cx: &mut Ctx, members: &[(DefId, Vec<TyId>)]) -> FxHashMap<DefId, Expansive> {
    let mut edges: Vec<TypeEdge> = vec![];
    for (d, tys) in members {
        if generic_names(cx, *d).is_empty() {
            continue;
        }
        for t in tys {
            named_in(cx, *t, &mut |g, args, at| {
                for (j, a) in args.iter().enumerate() {
                    let mut ps = vec![];
                    collect_params(&cx.ty, *a, &mut ps);
                    for p in ps {
                        let bare = matches!(cx.ty.kind(*a), TyKind::Param(q) if *q == p);
                        edges.push(((*d, p), (g, j as u32), !bare, at));
                    }
                }
            });
        }
    }
    let mut out = FxHashMap::default();
    if !edges.iter().any(|e| e.2) {
        return out;
    }
    let pairs: Vec<_> = edges.iter().map(|e| (e.0, e.1)).collect();
    let graph = Graph::of(&pairs);
    let comp = graph.components();
    let growing: FxHashSet<usize> = (edges.iter())
        .filter(|(from, to, grows, _)| *grows && comp[graph.index[from]] == comp[graph.index[to]])
        .map(|(from, _, _, _)| comp[graph.index[from]])
        .collect();
    // The definitions in those components, each with an edge of its own there.
    let mut defs: Vec<(DefId, TyId)> = vec![];
    for (from, to, _, at) in &edges {
        let c = comp[graph.index[from]];
        if growing.contains(&c) && comp[graph.index[to]] == c && !defs.iter().any(|x| x.0 == from.0)
        {
            defs.push((from.0, *at));
        }
    }
    let in_cycle: FxHashSet<DefId> = defs.iter().map(|x| x.0).collect();
    for (d, at) in defs {
        let names = generic_names(cx, d).to_vec();
        let params: Vec<TyId> = (0..names.len() as u32).map(|p| cx.ty.param(p)).collect();
        let own_ty = match &cx.info[d.0 as usize] {
            DefInfo::Iface(_) => cx.ty.intern(TyKind::Dyn(d, params.clone())),
            _ => cx.ty.intern(TyKind::Adt(d, params.clone())),
        };
        // Where `d` meets itself again: in a member, or a member of a member in its cycle.
        let grown = self_mention(cx, members, &in_cycle, d, &params).unwrap_or(at);
        let own = cx.display_in(own_ty, &names);
        let grown = cx.display_in(grown, &names);
        out.insert(d, Expansive { own, grown });
    }
    out
}

/// The first type `d<args>` (other than `d`'s own parameters) that `d`'s members reach within
/// three steps through definitions of its cycle, substituted.
fn self_mention(
    cx: &mut Ctx,
    members: &[(DefId, Vec<TyId>)],
    in_cycle: &FxHashSet<DefId>,
    d: DefId,
    params: &[TyId],
) -> Option<TyId> {
    let of = |g: DefId| members.iter().find(|m| m.0 == g).map(|m| m.1.clone());
    let mut level: Vec<TyId> = of(d)?;
    for _ in 0..3 {
        let mut next = vec![];
        for t in level {
            let mut found = vec![];
            named_in(cx, t, &mut |g, args, at| found.push((g, args.to_vec(), at)));
            for (g, args, at) in found {
                if g == d && args != params {
                    return Some(at);
                }
                if g != d && in_cycle.contains(&g) {
                    for m in of(g).unwrap_or_default() {
                        next.push(cx.subst(m, &args));
                    }
                }
            }
        }
        level = next;
    }
    None
}

/// The expansive definition `t` reaches through a definition of `bad`, if any.
fn reaches(cx: &Ctx, t: TyId, bad: &FxHashMap<DefId, DefId>) -> Option<DefId> {
    let mut found = None;
    named_in(cx, t, &mut |g, _, _| {
        if found.is_none() {
            found = bad.get(&g).copied();
        }
    });
    found
}

/// Where the function `d` first mentions a type that reaches an expansive definition: the span,
/// the type, and the generic function called with it when the mention is a call's type argument
/// (preferred over any other mention).
fn first_mention(
    cx: &mut Ctx,
    d: DefId,
    bad: &FxHashMap<DefId, DefId>,
) -> Option<(Span, TyId, Option<DefId>)> {
    let DefInfo::Fn(info) = &cx.info[d.0 as usize] else {
        return None;
    };
    let sig: Vec<(TyId, Span)> = (info.params.iter())
        .map(|p| (p.ty, p.span))
        .chain(std::iter::once((
            info.ret,
            info.ret_span.unwrap_or(info.span),
        )))
        .collect();
    let mut first: Option<(Span, TyId)> = None;
    let mut call: Option<(Span, TyId, DefId)> = None;
    if let Some(Some(Def::Fn(f))) = cx.defs.get_mut(d.0 as usize) {
        let tys = &cx.ty;
        let hit = |t: TyId| {
            let mut found = false;
            named_in_types(tys, t, &mut |g| found |= bad.contains_key(&g));
            found
        };
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            if call.is_some() {
                return;
            }
            if let E::Call {
                callee: Callee::Def(g, targs),
                ..
            } = &e.kind
            {
                if let Some(t) = targs.iter().copied().find(|t| hit(*t)) {
                    call = Some((e.span, t, *g));
                    return;
                }
            }
            if first.is_none() && hit(e.ty) {
                first = Some((e.span, e.ty));
            }
        });
    }
    if let Some((span, t, g)) = call {
        return Some((span, t, Some(g)));
    }
    if let Some((span, t)) = first {
        return Some((span, t, None));
    }
    let (t, span) = sig
        .into_iter()
        .find(|(t, _)| reaches(cx, *t, bad).is_some())?;
    Some((span, t, None))
}

/// [`named_in`] over the type table alone (inside a visit of a body, which borrows the defs).
fn named_in_types(tys: &crate::types::Types, t: TyId, f: &mut impl FnMut(DefId)) {
    let k = tys.kind(t);
    if let TyKind::Adt(g, _) | TyKind::Dyn(g, _) = k {
        f(*g);
    }
    for c in children(k) {
        named_in_types(tys, c, f);
    }
}

fn report(
    cx: &mut Ctx,
    expansive: &FxHashMap<DefId, Expansive>,
    bad: &FxHashMap<DefId, DefId>,
    span: Span,
    (ty, shown): (TyId, String),
    call: Option<DefId>,
) {
    let Some(root) = reaches(cx, ty, bad) else {
        return;
    };
    let e = &expansive[&root];
    let what = match call.map(|g| callee_name(cx, g)) {
        Some(n) if n == "JSON.parse" => "so `JSON.parse` cannot read it".to_string(),
        Some(n) if n == "JSON.stringify" => "so `JSON.stringify` cannot write it".to_string(),
        Some(n) if COMPARERS.contains(&short(&n)) => {
            format!("so `{}` cannot compare it", short(&n))
        }
        Some(n) => format!("so `{n}` cannot be compiled for `{shown}`"),
        None => format!("so a value of `{shown}` cannot be compiled"),
    };
    cx.error(
        Diagnostic::error(
            format!(
                "`{}` refers to itself at a growing type argument (`{}`), {what}",
                e.own, e.grown
            ),
            span,
        )
        .with_note(format!(
            "Velt compiles each instantiation of a type separately, and each `{}` needs one with a longer type argument, without end (TypeScript accepts the type); give the member a type that does not grow, or a `JsonValue`",
            e.own
        )),
    );
}

/// The functions and methods (by their own name) that compare values by their contents.
const COMPARERS: &[&str] = &["deepEqual", "toEqual", "assertEq"];

/// A method's own name (`toEqual` of `Expect.toEqual`).
fn short(n: &str) -> &str {
    n.rsplit('.').next().unwrap_or(n)
}

/// A function's name as users write it: `JSON.parse` for a static method, `deepEqual` for a
/// function.
fn callee_name(cx: &Ctx, g: DefId) -> String {
    let name = match &cx.defs[g.0 as usize] {
        Some(Def::Fn(f)) => f.name.as_str(),
        _ => return "?".into(),
    };
    let name = name.split("::{closure").next().unwrap_or(name);
    let mut parts = name.rsplit("::");
    let last = parts.next().unwrap_or(name);
    match parts.next() {
        Some(owner) if owner.starts_with(|c: char| c.is_ascii_uppercase()) => {
            format!("{owner}.{last}")
        }
        _ => last.to_string(),
    }
}
