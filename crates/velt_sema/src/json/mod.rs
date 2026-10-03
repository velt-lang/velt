//! What `JSON.stringify` / `JSON.parse<T>` can be generated for (after all bodies): numbers,
//! `bool`, `string`, arrays, `T | null`, C-like enums, unions (stringify only), structs / classes
//! / object literals whose fields are all public and serializable, and the prelude's `JsonValue`.
//! A type with a private field has no JSON form: std types keep runtime handles (pointers) in
//! private fields, and decoding one from untrusted input would forge it. A class with a private
//! or protected constructor can be written but not decoded: decoding fills the fields without
//! running a constructor, which would bypass the class's factories and their checks. The intrinsics sit in generic prelude code
//! (`JSON.stringify<T>`), so a requirement on a type parameter propagates to every caller (and
//! from a closure to its enclosing function) until it meets a concrete type, which is checked at
//! that call site. A method dispatched dynamically (through an interface or a base class) has
//! no call with type arguments: its requirements are instantiated wherever its class type is
//! mentioned ([`crate::dispatch`]).

use std::collections::{HashMap, HashSet};

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::dispatch::{instantiate, Dispatch};
use crate::hir::{AdtKind, Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, TyId, TyKind};
use crate::types::children;
use crate::visit;

mod union;

use union::union_decode_problem;

/// JSON-relevant facts of one function body.
#[derive(Default)]
struct Uses {
    /// Types given to the JSON intrinsics directly (`true`: decoded by `JSON.parse`).
    direct: Vec<(TyId, Span, bool)>,
    calls: Vec<(DefId, Vec<TyId>, Span)>,
    closures: Vec<DefId>,
    /// Every type the body mentions (expressions and type arguments of calls), with its first
    /// span: the methods of class types among them may be dispatched dynamically.
    types: Vec<(TyId, Span)>,
}

/// Types (and whether `JSON.parse` decodes them) a function needs to have a JSON form.
type Needs = HashMap<DefId, Vec<(TyId, bool)>>;

pub(crate) fn check_json_types(cx: &mut Ctx) {
    let uses = collect(cx);
    let mut needs: Needs = HashMap::new();
    let mut dispatch = Dispatch::default();
    let mut checked: HashSet<(TyId, Span, bool)> = HashSet::new();
    // A type without a JSON form is reported once per place, whether it is parsed, written or
    // both (`JSON.stringify(JSON.parse<T>(s))`).
    let mut reported: HashSet<(TyId, Span)> = HashSet::new();
    let mut changed = true;
    while changed {
        changed = false;
        for (f, u) in &uses {
            let mut reqs: Vec<(TyId, Span, bool)> = u.direct.clone();
            for (d, targs, span) in &u.calls {
                for (t, parse) in needs.get(d).cloned().unwrap_or_default() {
                    reqs.push((cx.ty.subst(t, targs), *span, parse));
                }
            }
            for c in &u.closures {
                for (t, parse) in needs.get(c).cloned().unwrap_or_default() {
                    reqs.push((t, cx.def_spans[c.0 as usize], parse));
                }
            }
            reqs.extend(dispatched(cx, u, &needs, &mut dispatch));
            for (t, span, parse) in reqs {
                if has_param(cx, t) {
                    let n = needs.entry(*f).or_default();
                    if !n.contains(&(t, parse)) {
                        n.push((t, parse));
                        changed = true;
                    }
                } else if !reported.contains(&(t, span))
                    && checked.insert((t, span, parse))
                    && check(cx, t, span, parse)
                {
                    reported.insert((t, span));
                }
            }
        }
    }
}

/// The types the methods that the class types `u` mentions dispatch dynamically to need,
/// instantiated with those types' arguments.
fn dispatched(
    cx: &mut Ctx,
    u: &Uses,
    needs: &Needs,
    dispatch: &mut Dispatch,
) -> Vec<(TyId, Span, bool)> {
    let mut reqs = vec![];
    for (t, span) in &u.types {
        for (m, args) in dispatch.targets(cx, *t) {
            for (need, parse) in needs.get(&m).cloned().unwrap_or_default() {
                if let Some(need) = instantiate(cx, need, &args) {
                    reqs.push((need, *span, parse));
                }
            }
        }
    }
    reqs
}

fn collect(cx: &mut Ctx) -> Vec<(DefId, Uses)> {
    let mut out = vec![];
    for (i, d) in cx.defs.iter_mut().enumerate() {
        let Some(Def::Fn(f)) = d else { continue };
        let mut u = Uses::default();
        let mut seen: HashSet<TyId> = HashSet::new();
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            if seen.insert(e.ty) {
                u.types.push((e.ty, e.span));
            }
            // Type arguments of generic calls and of generic functions used as values.
            let targs = match &e.kind {
                E::Call {
                    callee: Callee::Def(_, targs),
                    ..
                }
                | E::FnRef(_, targs) => targs.as_slice(),
                _ => &[],
            };
            for t in targs {
                if seen.insert(*t) {
                    u.types.push((*t, e.span));
                }
            }
            uses_of(&mut u, e);
        });
        out.push((DefId(i as u32), u));
    }
    out
}

/// The JSON intrinsic calls, generic calls and closures of one expression.
fn uses_of(u: &mut Uses, e: &Expr) {
    match &e.kind {
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::JsonStringify),
            args,
        } => {
            if let Some(a) = args.first() {
                u.direct.push((a.ty, e.span, false));
            }
        }
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::JsonParse),
            ..
        } => u.direct.push((e.ty, e.span, true)),
        E::Call {
            callee: Callee::Def(d, targs),
            ..
        } if !targs.is_empty() => u.calls.push((*d, targs.clone(), e.span)),
        // A generic function used as a value (`const f: (s: string) => T = decode`) needs what
        // its calls would need at those type arguments.
        E::FnRef(d, targs) if !targs.is_empty() => u.calls.push((*d, targs.clone(), e.span)),
        E::Closure(c) => u.closures.push(*c),
        _ => {}
    }
}

fn has_param(cx: &Ctx, t: TyId) -> bool {
    match cx.ty.kind(t) {
        TyKind::Param(_) => true,
        k => children(k).into_iter().any(|c| has_param(cx, c)),
    }
}

/// Reports `t` at `span` if it has no JSON form (`true`: reported).
fn check(cx: &mut Ctx, t: TyId, span: Span, parse: bool) -> bool {
    let mut stack = vec![];
    let Some(bad) = unserializable(cx, t, &mut stack, parse) else {
        return false;
    };
    let (tn, bn) = (cx.display(t), cx.display(bad));
    if let Some(why) = cx
        .union_def(bad)
        .and_then(|_| union_decode_problem(cx, bad))
    {
        let within = if t == bad {
            String::new()
        } else {
            format!(" (in `{tn}`)")
        };
        let fix = if why.contains("objects") {
            "give each object member a literal field such as `kind: \"a\"` (a discriminant), or parse a `JsonValue` and build the union from it"
        } else if why.contains("are both the JSON") {
            "give each member its own values, or parse a `JsonValue` and build the union from it"
        } else {
            "parse a `JsonValue` and build the union from it"
        };
        cx.error(
            Diagnostic::error(
                format!("`JSON.parse` cannot decode the union `{bn}`{within}: {why}"),
                span,
            )
            .with_note(fix),
        );
        return true;
    }
    if parse && report_restricted_ctor(cx, t, bad, span) {
        return true;
    }
    if matches!(cx.ty.kind(bad), TyKind::Adt(d, _) if cx.is_generator_class(*d)) {
        let what = match t == bad {
            true => format!("`{tn}` is a generator"),
            false => format!("`{tn}` contains the generator `{bn}`"),
        };
        cx.error(
            Diagnostic::error(format!("cannot convert to or from JSON: {what}"), span).with_note(
                "TypeScript allows this (`JSON.stringify` writes a generator as `{}`); Velt doesn't because a generator is a paused computation, not data; write the values it yields instead (collect them into an array with `for...of`)",
            ),
        );
        return true;
    }
    let private = private_field(cx, bad);
    let what = match (&private, t == bad) {
        (Some((field, _)), true) => {
            format!("`{tn}` has a private field `{field}`, so it has no JSON form")
        }
        (Some((field, _)), false) => format!(
            "`{tn}` contains `{bn}`, which has a private field `{field}`, so it has no JSON form"
        ),
        (None, true) => format!("`{tn}` has no JSON form"),
        (None, false) => format!("`{tn}` contains `{bn}`, which has no JSON form"),
    };
    let mut d = Diagnostic::error(format!("cannot convert to or from JSON: {what}"), span);
    d = match private {
        Some((_, field_span)) => d.with_label(field_span, "private field").with_note(
            "a type with private fields (such as a runtime handle) has no JSON form; convert it to a type with public fields first",
        ),
        None => d.with_note(
            "JSON supports numbers, boolean, string, literal types, enums, arrays, tuples, `T | null`, `Map<string, T>`, structs, classes and object literals of those, and `JsonValue`",
        ),
    };
    if matches!(cx.ty.kind(bad), TyKind::Adt(d, _) if Some(*d) == cx.prelude_adt("Map")) {
        d = d.with_note("a `Map` converts to a JSON object only with `string` keys");
    }
    cx.error(d);
    true
}

/// Reports decoding `bad` (part of `t`) if its constructor is private or protected (`true`:
/// reported).
fn report_restricted_ctor(cx: &mut Ctx, t: TyId, bad: TyId, span: Span) -> bool {
    let TyKind::Adt(d, _) = cx.ty.kind(bad) else {
        return false;
    };
    let visibility = match cx.ctor_visibility(*d) {
        ast::CtorVisibility::Public => return false,
        ast::CtorVisibility::Private => "private",
        ast::CtorVisibility::Protected => "protected",
    };
    let name = cx.adt(*d).map(|a| a.name.clone()).unwrap_or_default();
    let within = if t == bad {
        String::new()
    } else {
        format!(" (in `{}`)", cx.display(t))
    };
    let article = if name.starts_with(['A', 'E', 'I', 'O', 'U']) {
        "an"
    } else {
        "a"
    };
    cx.error(
        Diagnostic::error(
            format!("`JSON.parse` cannot create {article} `{name}`{within}: its constructor is {visibility}"),
            span,
        )
        .with_note(match cx.factories(*d) {
            Some(f) => format!("decoding fills the fields without running a constructor; decode into a plain object type and call {f}"),
            None => format!("decoding fills the fields without running a constructor; decode into a plain object type and create the `{name}` from it with a static factory method"),
        }),
    );
    true
}

/// The first private field of struct or class type `t` (name and declaration), if it has one.
fn private_field(cx: &Ctx, t: TyId) -> Option<(String, Span)> {
    let TyKind::Adt(d, _) = cx.ty.kind(t) else {
        return None;
    };
    let d = *d;
    // The prelude's `Map` and `Record` have their own rules (and private fields of their own).
    if Some(d) == cx.prelude_adt("Map") || Some(d) == cx.prelude_adt("Record") {
        return None;
    }
    match &cx.info[d.0 as usize] {
        DefInfo::Adt(a) => a
            .fields
            .iter()
            .find(|f| f.private_to.is_some())
            .map(|f| (f.name.clone(), f.span)),
        _ => None,
    }
}

/// Lowering (`velt_vir` lower/json read.rs and write.rs) handles exactly what this accepts:
/// keep them in sync, or a type let through here is an internal error there.
/// The first part of `t` without a JSON form, if any (`stack`: ADTs being visited, so recursive
/// types terminate). Unions can be written (as their active member) and decoded when their
/// members can be told apart.
fn unserializable(cx: &mut Ctx, t: TyId, stack: &mut Vec<TyId>, parse: bool) -> Option<TyId> {
    let fields: Vec<TyId> = match cx.ty.kind(t).clone() {
        TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Str => return None,
        // Already reported.
        TyKind::Error => return None,
        // Literal types are their value (decoding checks it).
        TyKind::Literal(_) => return None,
        // Elements of `never[]` (`JSON.stringify([])`) never exist.
        TyKind::Never if !parse => return None,
        TyKind::Array(e) | TyKind::Option(e) => vec![e],
        // Tuples are fixed-length arrays.
        TyKind::Tuple(es) => es,
        TyKind::Adt(d, args) => {
            if stack.contains(&t) || cx.is_json_value(d) {
                return None;
            }
            // `Record<K, V>` is an object (`record_keys` checked `K` where the type was resolved
            // or instantiated, before this check).
            if Some(d) == cx.prelude_adt("Record") {
                return match args.as_slice() {
                    [_, v] => unserializable(cx, *v, stack, parse),
                    _ => Some(t),
                };
            }
            // `Map<string, V>` is an object; other keys have no JSON form.
            if Some(d) == cx.prelude_adt("Map") {
                return match args.as_slice() {
                    [k, v] if matches!(cx.ty.kind(*k), TyKind::Str) => {
                        unserializable(cx, *v, stack, parse)
                    }
                    _ => Some(t),
                };
            }
            // Decoding would create an instance without running its non-public constructor.
            if parse && cx.ctor_visibility(d) != ast::CtorVisibility::Public {
                return Some(t);
            }
            let tys: Vec<TyId> = match &cx.info[d.0 as usize] {
                // Private fields hold what a type keeps to itself (runtime handles in std).
                DefInfo::Adt(a) if a.fields.iter().any(|f| f.private_to.is_some()) => {
                    return Some(t)
                }
                DefInfo::Adt(a)
                    if matches!(a.kind, AdtKind::Struct | AdtKind::Class | AdtKind::Anon) =>
                {
                    a.fields.iter().map(|f| f.ty).collect()
                }
                // Numeric enums are numbers (their discriminants), string enums their strings.
                DefInfo::Enum(e) if e.variants.iter().all(|v| v.payload.is_empty()) => return None,
                // A union is written as its active member; decoding tells the members apart by
                // the JSON value (see `union_decode_problem`).
                DefInfo::Enum(e) if e.is_union => {
                    let members = e.variants.iter().map(|v| v.payload[0]).collect();
                    if parse && union_decode_problem(cx, t).is_some() {
                        return Some(t);
                    }
                    members
                }
                _ => return Some(t),
            };
            stack.push(t);
            let tys: Vec<TyId> = tys.into_iter().map(|f| cx.ty.subst(f, &args)).collect();
            let bad = tys
                .into_iter()
                .find_map(|f| unserializable(cx, f, stack, parse));
            stack.pop();
            return bad;
        }
        _ => return Some(t),
    };
    fields
        .into_iter()
        .find_map(|f| unserializable(cx, f, stack, parse))
}
