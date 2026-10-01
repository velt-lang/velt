//! What `JSON.stringify` / `JSON.parse<T>` can be generated for (after all bodies): numbers,
//! `bool`, `string`, arrays, `T | null`, C-like enums, unions (stringify only), structs / classes / object literals whose fields are
//! all serializable, and the prelude's `JsonValue`. The intrinsics sit in generic prelude code
//! (`JSON.stringify<T>`), so a requirement on a type parameter propagates to every caller (and
//! from a closure to its enclosing function) until it meets a concrete type, which is checked at
//! that call site.

use std::collections::{HashMap, HashSet};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{
    AdtKind, Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, LitValue, TyId, TyKind,
};
use crate::types::children;
use crate::visit;

/// JSON-relevant facts of one function body.
#[derive(Default)]
struct Uses {
    /// Types given to the JSON intrinsics directly (`true`: decoded by `JSON.parse`).
    direct: Vec<(TyId, Span, bool)>,
    calls: Vec<(DefId, Vec<TyId>, Span)>,
    closures: Vec<DefId>,
}

pub(crate) fn check_json_types(cx: &mut Ctx) {
    let uses = collect(cx);
    let mut needs: HashMap<DefId, Vec<(TyId, bool)>> = HashMap::new();
    let mut checked: HashSet<(TyId, Span, bool)> = HashSet::new();
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
            for (t, span, parse) in reqs {
                if has_param(cx, t) {
                    let n = needs.entry(*f).or_default();
                    if !n.contains(&(t, parse)) {
                        n.push((t, parse));
                        changed = true;
                    }
                } else if checked.insert((t, span, parse)) {
                    check(cx, t, span, parse);
                }
            }
        }
    }
}

fn collect(cx: &mut Ctx) -> Vec<(DefId, Uses)> {
    let mut out = vec![];
    for (i, d) in cx.defs.iter_mut().enumerate() {
        let Some(Def::Fn(f)) = d else { continue };
        let mut u = Uses::default();
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| match &e.kind {
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
            E::Closure(c) => u.closures.push(*c),
            _ => {}
        });
        out.push((DefId(i as u32), u));
    }
    out
}

fn has_param(cx: &Ctx, t: TyId) -> bool {
    match cx.ty.kind(t) {
        TyKind::Param(_) => true,
        k => children(k).into_iter().any(|c| has_param(cx, c)),
    }
}

fn check(cx: &mut Ctx, t: TyId, span: Span, parse: bool) {
    let mut stack = vec![];
    if let Some(bad) = unserializable(cx, t, &mut stack, parse) {
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
            return;
        }
        let what = if t == bad {
            format!("`{tn}` has no JSON form")
        } else {
            format!("`{tn}` contains `{bn}`, which has no JSON form")
        };
        let mut d =
            Diagnostic::error(format!("cannot convert to or from JSON: {what}"), span).with_note(
                "JSON supports numbers, bool, string, literal types, enums, arrays, tuples, `T | null`, `Map<string, T>`, structs, classes and object literals of those, and `JsonValue`",
            );
        if matches!(cx.ty.kind(bad), TyKind::Adt(d, _) if Some(*d) == cx.prelude_adt("Map")) {
            d = d.with_note("a `Map` converts to a JSON object only with `string` keys");
        }
        cx.error(d);
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
            // `Record<K, V>` is an object (its key type was checked where it was built).
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
            let tys: Vec<TyId> = match &cx.info[d.0 as usize] {
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

/// How a union member is recognized in a JSON document (the decoder's view; velt_vir
/// lower/json/union.rs classifies members the same way).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Str,
    /// String literal types and string enums: matched by value.
    StrLits,
    Num,
    NumLits,
    Bool,
    BoolLits,
    Array,
    /// Structs, classes and object literal types.
    Object,
    /// `Map<string, V>` / `Record<K, V>`: any object.
    Dict,
    /// `JsonValue`: any value.
    Any,
}

fn shape(cx: &Ctx, m: TyId) -> Shape {
    match cx.ty.kind(m) {
        TyKind::Str => Shape::Str,
        TyKind::Int(_) | TyKind::Float(_) => Shape::Num,
        TyKind::Bool => Shape::Bool,
        TyKind::Literal(LitValue::Str(_)) => Shape::StrLits,
        TyKind::Literal(LitValue::Bool(_)) => Shape::BoolLits,
        TyKind::Literal(_) => Shape::NumLits,
        TyKind::Array(_) | TyKind::Tuple(_) => Shape::Array,
        TyKind::Adt(d, _) if cx.is_json_value(*d) => Shape::Any,
        TyKind::Adt(d, _)
            if Some(*d) == cx.prelude_adt("Map") || Some(*d) == cx.prelude_adt("Record") =>
        {
            Shape::Dict
        }
        TyKind::Adt(d, _) => match cx.enum_info(*d) {
            Some(e) if e.variants.iter().any(|v| v.str_value.is_some()) => Shape::StrLits,
            Some(_) => Shape::NumLits,
            None => Shape::Object,
        },
        _ => Shape::Object,
    }
}

/// Why `JSON.parse` cannot decode union `u` (`None`: it can). Members are told apart by the
/// kind of JSON value; literal members by value; several object members by a discriminant (a
/// field with a different literal type in each) or else by a required key only one of them has.
pub(crate) fn union_decode_problem(cx: &mut Ctx, u: TyId) -> Option<String> {
    let members = cx.union_members(u)?;
    let shapes: Vec<Shape> = members.iter().map(|m| shape(cx, *m)).collect();
    let of = |want: &[Shape]| -> Vec<TyId> {
        members
            .iter()
            .zip(&shapes)
            .filter(|(_, s)| want.contains(s))
            .map(|(m, _)| *m)
            .collect()
    };
    let both = |cx: &mut Ctx, ms: &[TyId], what: &str| {
        let (a, b) = (cx.display(ms[0]), cx.display(ms[1]));
        format!("`{a}` and `{b}` are both {what}")
    };
    if shapes.contains(&Shape::Any) {
        return Some("a `JsonValue` member takes any JSON value".into());
    }
    let nums = of(&[Shape::Num]);
    if nums.len() > 1 {
        return Some(both(cx, &nums, "JSON numbers"));
    }
    let arrays = of(&[Shape::Array]);
    if arrays.len() > 1 {
        return Some(both(cx, &arrays, "JSON arrays"));
    }
    let objects = of(&[Shape::Object, Shape::Dict]);
    if objects.len() > 1 {
        if let Some(dict) = of(&[Shape::Dict]).first() {
            let dn = cx.display(*dict);
            return Some(format!("`{dn}` takes any JSON object"));
        }
        if object_discriminant(cx, &objects).is_none() && required_keys(cx, &objects).is_none() {
            return Some(both(
                cx,
                &objects,
                "objects with no discriminant field and no required field that only one of them has",
            ));
        }
    }
    None
}

/// Field names of object type `t` with whether each is required (not `T | null`).
fn object_fields(cx: &mut Ctx, t: TyId) -> Vec<(String, TyId, bool)> {
    let TyKind::Adt(d, args) = cx.ty.kind(t).clone() else {
        return vec![];
    };
    let fields: Vec<(String, TyId)> = cx
        .adt(d)
        .map(|a| a.fields.iter().map(|f| (f.name.clone(), f.ty)).collect())
        .unwrap_or_default();
    fields
        .into_iter()
        .map(|(n, ft)| {
            let ft = cx.ty.subst(ft, &args);
            let required = !matches!(cx.ty.kind(ft), TyKind::Option(_));
            (n, ft, required)
        })
        .collect()
}

/// A field every object member has with a literal type, the values all different.
pub(crate) fn object_discriminant(cx: &mut Ctx, objects: &[TyId]) -> Option<String> {
    let first = object_fields(cx, objects[0]);
    'names: for (name, _, _) in first {
        let mut seen: Vec<LitValue> = vec![];
        for &m in objects {
            let Some((_, ft)) = cx.field_of(m, &name) else {
                continue 'names;
            };
            let Some(v) = cx.lit_value(ft) else {
                continue 'names;
            };
            if seen.contains(&v) {
                continue 'names;
            }
            seen.push(v);
        }
        return Some(name);
    }
    None
}

/// For each object member, a required field no other member has.
pub(crate) fn required_keys(cx: &mut Ctx, objects: &[TyId]) -> Option<Vec<String>> {
    let fields: Vec<Vec<(String, TyId, bool)>> =
        objects.iter().map(|m| object_fields(cx, *m)).collect();
    let mut keys = vec![];
    for (i, fs) in fields.iter().enumerate() {
        let own = fs.iter().find(|(n, _, required)| {
            *required
                && fields
                    .iter()
                    .enumerate()
                    .all(|(j, other)| j == i || other.iter().all(|(o, _, _)| o != n))
        })?;
        keys.push(own.0.clone());
    }
    Some(keys)
}
