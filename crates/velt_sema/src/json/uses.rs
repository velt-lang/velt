//! The JSON-relevant uses of one expression (`check_json_types`' collection).

use crate::hir::{Callee, DefId, Expr, ExprKind as E, Intrinsic, TyId, TyKind};
use crate::object_copies::Seen;
use rustc_hash::FxHashSet;

use super::{param_in, Need, PtrUse, Uses};

/// The qualified name's end of the generated function `JSON.parse<T>` calls.
pub(super) const PARSE_TOP: &str = "::parseJsonTop";

/// The decoding calls, generic calls and closures of one expression, and with `looks` its
/// `console.log` and `===` calls. `parsers`: the generated `parseJsonTop` (a call of it
/// decodes its `T`).
pub(super) fn uses_of(
    tys: &crate::types::Types,
    u: &mut Uses,
    e: &Expr,
    looks: bool,
    parsers: &FxHashSet<DefId>,
) {
    // A type parameter read without a lock: whatever it is instantiated with may hold one.
    let unlocked = match &e.kind {
        E::ToDyn { expr, .. } => Some(expr.as_ref()),
        E::Call {
            callee:
                Callee::Intrinsic(
                    Intrinsic::Print
                    | Intrinsic::PrintErr
                    | Intrinsic::ToString
                    | Intrinsic::TemplateToString,
                ),
            args,
        } => args.first(),
        _ => None,
    };
    if let E::Call {
        callee: Callee::Intrinsic(Intrinsic::Print | Intrinsic::PrintErr),
        args,
    } = &e.kind
    {
        for a in args.iter().skip(1).filter(|a| param_in(tys, a.ty)) {
            u.direct.push((a.ty, a.span, Need::NoLock));
        }
    }
    if let Some(a) = unlocked.filter(|a| param_in(tys, a.ty)) {
        u.direct.push((a.ty, a.span, Need::NoLock));
    }
    // A type parameter converted to or from `any`, or to text: it can't be a C pointer (#3094).
    let converted = match &e.kind {
        E::ToDyn { expr, .. } => Some((expr.ty, expr.span, PtrUse::ToDyn)),
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::ToString | Intrinsic::TemplateToString),
            args,
        } => args.first().map(|a| (a.ty, a.span, PtrUse::ToText)),
        // Only where an instance could put a pointer: the parameter itself or an element of
        // it (`a as T`, `a as T[]`). Recording an object type (`o as Box<T>`, a literal's view)
        // would substitute, and so make, every instance of it at every call.
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::AnyTo),
            ..
        } if param_element(tys, e.ty) => Some((e.ty, e.span, PtrUse::FromDyn)),
        _ => None,
    };
    if let Some((t, span, how)) = converted.filter(|(t, ..)| param_in(tys, *t)) {
        u.direct.push((t, span, Need::NoPtr(how)));
    }
    match &e.kind {
        E::Call {
            callee: Callee::Def(d, _),
            ..
        } if parsers.contains(d) => u.direct.push((e.ty, e.span, Need::Json(true))),
        E::Call {
            callee:
                Callee::Intrinsic(
                    i @ (Intrinsic::Print
                    | Intrinsic::PrintErr
                    | Intrinsic::Same
                    | Intrinsic::SameValue
                    | Intrinsic::KeyEq
                    | Intrinsic::IncludesEq),
                ),
            args,
        } if looks => {
            let how = match i {
                // `IncludesEq`: `includes`; `KeyEq`: `Map` keys, which `Hash` shows too.
                Intrinsic::Same
                | Intrinsic::SameValue
                | Intrinsic::KeyEq
                | Intrinsic::IncludesEq => Seen::Identity,
                _ => Seen::Printed,
            };
            for a in args {
                u.direct.push((a.ty, a.span, Need::Seen(how)));
            }
        }
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

/// Is `t` a type parameter, or an array, tuple or option with one as an element (at any depth)?
fn param_element(tys: &crate::types::Types, t: TyId) -> bool {
    match tys.kind(t) {
        TyKind::Param(_) => true,
        TyKind::Array(e) | TyKind::Option(e, _) => param_element(tys, *e),
        TyKind::Tuple(es) => es.iter().any(|&e| param_element(tys, e)),
        _ => false,
    }
}
