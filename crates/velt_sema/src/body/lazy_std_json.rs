//! When the JSON glue's modules ([`JSON_GLUE`](super::JSON_GLUE)) are reached (#1658,
//! docs/internals/design/lazy-std.md). Lowering builds a JSON decoder or encoder, which calls
//! `std/json/reader` or `std/json/writer` by name, for each instantiation of a JSON intrinsic
//! ([`json_glue`]) it builds. An intrinsic whose types are concrete is such an instantiation. One
//! that mentions the type parameters of the function it is in (`JSON.parse<T>`'s
//! `__intrinsic_json_parse<T>`, which `JsonValue.as<T>` calls in turn) is one only once a call
//! instantiates that function with concrete types, so the module is reached at such a call, not
//! when the generic function is checked: a prelude method that is always checked but that no
//! program calls (`JsonValue.as<T>`) then costs hello world nothing.

use rustc_hash::FxHashSet;

use super::Scan;
use crate::ctx::Ctx;
use crate::defs::{DefInfo, FnKind};
use crate::hir::{DefId, Intrinsic, TyId, TyKind};

/// The [`JSON_GLUE`](super::JSON_GLUE) module the glue lowering builds for this intrinsic calls: the reader for the
/// decoders, the writer for the encoders (`JSON.stringify` and a driver's parameters, which it
/// may bind as JSON text).
pub(super) fn json_glue(i: &Intrinsic) -> Option<usize> {
    match i {
        Intrinsic::JsonParse
        | Intrinsic::JsonParseRows
        | Intrinsic::JsonParseCursor
        | Intrinsic::ReadRows
        | Intrinsic::ReadRow => Some(0),
        Intrinsic::DbBind => Some(1),
        _ => None,
    }
}

/// What decides when the [`JSON_GLUE`](super::JSON_GLUE) modules are reached, across [`super::reach`]'s rounds.
#[derive(Default)]
pub(crate) struct JsonReach {
    /// Per module: its deferred definitions, until it is reached (then empty).
    pub(super) defs: [Vec<DefId>; 2],
    /// Per module: whether it is reached.
    on: [bool; 2],
    /// Per module: the generic functions that instantiate its glue for some of their type
    /// arguments (they mention them in an intrinsic, or in a call of such a function).
    generic: [FxHashSet<DefId>; 2],
    /// Calls with type arguments in scanned code: caller (none in a default), callee, and
    /// whether the type arguments are concrete.
    calls: Vec<(Option<DefId>, DefId, bool)>,
    /// Whether scanned code calls a generic method through an interface or dynamically, where
    /// the callee is not named.
    generic_dispatch: bool,
}

/// Is every type parameter `d`'s code can mention one of its own (a generic function, or a
/// generic method of a type that has none)? Then the type arguments of a call name its
/// instantiation.
fn own_params_only(cx: &Ctx, d: DefId) -> bool {
    let DefInfo::Fn(f) = &cx.info[d.0 as usize] else {
        return false;
    };
    matches!(f.kind, FnKind::Free | FnKind::Static | FnKind::Method)
        && !f.generics.names.is_empty()
        && f.owner.is_none_or(|o| match &cx.info[o.0 as usize] {
            DefInfo::Adt(a) => a.generics.names.is_empty(),
            _ => false,
        })
}

fn mentions_params(cx: &Ctx, ts: &[TyId]) -> bool {
    let mut found = false;
    for t in ts {
        cx.ty
            .visit(*t, &mut |k| found |= matches!(k, TyKind::Param(_)));
    }
    found
}

/// Take in what the last round scanned (`sc.json`, `sc.calls`), and add the deferred
/// definitions of each module reached by now to `sc.found`.
pub(super) fn settle(cx: &mut Ctx, sc: &mut Scan) {
    let mut on = cx.lazy.json.on;
    for (k, cur, tys) in std::mem::take(&mut sc.json) {
        match cur.filter(|d| own_params_only(cx, *d)) {
            Some(d) if mentions_params(cx, &tys) => {
                cx.lazy.json.generic[k].insert(d);
            }
            _ => on[k] = true,
        }
    }
    for (caller, callee, tys) in std::mem::take(&mut sc.calls) {
        let concrete = !mentions_params(cx, &tys);
        cx.lazy.json.calls.push((caller, callee, concrete));
    }
    cx.lazy.json.generic_dispatch |= sc.generic_dispatch;
    let mut j = std::mem::take(&mut cx.lazy.json);
    for (k, on) in on.iter_mut().enumerate() {
        while !*on {
            let mut grown = false;
            for &(caller, callee, concrete) in j.calls.iter() {
                if !j.generic[k].contains(&callee) {
                    continue;
                }
                match caller.filter(|c| !concrete && own_params_only(cx, *c)) {
                    Some(c) => grown |= j.generic[k].insert(c),
                    None => *on = true,
                }
            }
            let methods = (j.generic[k].iter()).any(
                |d| matches!(&cx.info[d.0 as usize], DefInfo::Fn(f) if f.kind == FnKind::Method),
            );
            *on |= j.generic_dispatch && methods;
            if !grown {
                break;
            }
        }
        if *on {
            sc.found.append(&mut j.defs[k]);
        }
    }
    j.on = on;
    cx.lazy.json = j;
}
