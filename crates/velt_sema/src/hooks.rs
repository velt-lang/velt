//! The methods a class can give the formatters and `JSON.stringify` (`hir::AdtDef::to_string`,
//! `to_json` and `inspect`): `toString()` for `String(x)` / `${x}`, `toJSON()` for
//! `JSON.stringify`, as in JS, and `__inspect()` for `console.log` (std's types print their
//! state that way, as Node's custom inspect does, never their runtime handles). Lowering calls
//! them from glue, so a hook is a plain method: an instance method of the class or a base class,
//! with no parameters or type parameters of its own, neither async nor a generator.

use crate::collect::{lookup_method, Found};
use crate::ctx::Ctx;
use crate::hir::{AdtKind, DefId, TyId};

/// The `toString()` hook's name.
pub(crate) const TO_STRING: &str = "toString";
/// The `toJSON()` hook's name.
pub(crate) const TO_JSON: &str = "toJSON";
/// The `__inspect()` hook's name.
pub(crate) const INSPECT: &str = "__inspect";

/// Class `d<args>`'s hook method `name` and its result type (substituted with `args`), if it
/// has one.
pub(crate) fn hook(cx: &mut Ctx, d: DefId, args: &[TyId], name: &str) -> Option<(DefId, TyId)> {
    if cx.adt(d)?.kind != AdtKind::Class {
        return None;
    }
    let found = lookup_method(cx, d, args, name)?;
    let Found::Class { owner, .. } = &found else {
        return None;
    };
    if found.is_static() {
        return None;
    }
    let owner_generics = cx.adt(*owner)?.generics.len();
    let m = found.def();
    let f = cx.fn_info(m);
    if !f.params.is_empty()
        || f.is_async
        || f.is_generator
        || f.is_async_gen
        || f.generics.len() != owner_generics
    {
        return None;
    }
    let ret = f.ret;
    let ret = cx.subst(ret, &found.owner_args());
    Some((m, ret))
}

/// [`hook`], for lowering (after all bodies): only a method that cannot throw, since glue has
/// nowhere to send an error (`toString` must also return a `string`).
pub(crate) fn final_hook(
    cx: &mut Ctx,
    d: DefId,
    args: &[TyId],
    name: &str,
) -> Option<(DefId, TyId)> {
    let (m, ret) = hook(cx, d, args, name)?;
    let throws = cx.fn_info(m).throws;
    if throws.is_some_and(|t| t != cx.ty.never) {
        return None;
    }
    if name == TO_STRING && ret != cx.ty.str_ {
        return None;
    }
    Some((m, ret))
}
