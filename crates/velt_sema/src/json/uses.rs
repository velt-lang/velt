//! The JSON-relevant uses of one expression (`check_json_types`' collection).

use crate::hir::{Callee, DefId, Expr, ExprKind as E, Intrinsic};
use crate::object_copies::Seen;
use rustc_hash::FxHashSet;

use super::{param_in, Need, Uses};

/// The qualified name's end of the generated function `JSON.parse<T>` calls.
pub(super) const PARSE_TOP: &str = "::parseJsonTop";

/// The JSON intrinsic calls, generic calls and closures of one expression, and with `looks`
/// its `console.log` and `===` calls. `parsers`: the generated `parseJsonTop` (a call of it
/// needs its `T` decoded); `in_generated`: the expression is generated code, whose
/// `jsonParse<T>` was needed by such a call already.
pub(super) fn uses_of(
    tys: &crate::types::Types,
    u: &mut Uses,
    e: &Expr,
    looks: bool,
    (parsers, in_generated): (&FxHashSet<DefId>, bool),
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
    match &e.kind {
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::JsonParse),
            ..
        } if !in_generated => u.direct.push((e.ty, e.span, Need::Json(true))),
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
