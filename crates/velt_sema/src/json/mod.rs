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
