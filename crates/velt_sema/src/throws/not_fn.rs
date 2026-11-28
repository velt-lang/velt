//! Node's `TypeError` for a call through `any` that holds no function (`o.ob()` with `o.ob`
//! a number), made in an async function outside every `catch` of it: the error rejects the
//! function's promise, as in Node, which an `await` in a `try` catches (velt_vir any_not_fn.rs
//! throws it to the function's own error). So does one in an arrow passed to a promise's
//! `then`, `catch` or `finally`: it rejects the promise the call returns.
//!
//! An async function (or such a callback) whose error type is open (inferred from its body) counts it among what it
//! throws ([`FnCx::note_any_call`](crate::body::FnCx) records the source). One whose error type
//! is written (`Promise<T> throws E`), fixed by the function type its closure is checked
//! against, or bound by the interface method it implements does not: the source is left out of
//! it here, before inference, so its own clause or bound holds as before, and velt_vir stops the
//! program with the message there, as it did before. Such a function that awaits an open one
//! does get it, as from any other error the awaited function throws (a clause without
//! `TypeError` reports it). A sync function keeps stopping: its errors would reach clauses
//! through every caller (#3955).

use velt_common::Span;

use super::groups::Groups;
use crate::ctx::Ctx;
use crate::defs::ThrowSrc;
use crate::hir::{DefId, TyId, TyKind};

/// The source a call through `any` records: the prelude's `TypeError` (`None` without one).
/// Its span is [`Span::DUMMY`], which tells it apart from a `throw` of a `TypeError`.
pub(crate) fn not_fn_src(cx: &mut Ctx) -> Option<ThrowSrc> {
    let d = cx.prelude_adt("TypeError")?;
    let te = cx.ty.intern(TyKind::Adt(d, vec![]));
    Some(ThrowSrc::Direct(te, Span::DUMMY))
}

/// Is `s` the source [`not_fn_src`] made (`te`: the prelude's `TypeError`)?
fn is_not_fn(s: &ThrowSrc, te: TyId) -> bool {
    matches!(s, ThrowSrc::Direct(t, span) if *t == te && *span == Span::DUMMY)
}

/// Leave the source out of the functions of `fns` whose error type is not open (module docs).
pub(super) fn keep_open_only(cx: &mut Ctx, fns: &[DefId], groups: &Groups) {
    let Some(ThrowSrc::Direct(te, _)) = not_fn_src(cx) else {
        return;
    };
    for &d in fns {
        let info = cx.fn_info(d);
        let fixed = info.declared_throws.is_some_and(|t| !t.from_body)
            || groups
                .group_of(d)
                .is_some_and(|g| groups.list[g].bound.is_some());
        if fixed && info.throw_srcs.iter().any(|s| is_not_fn(s, te)) {
            cx.fn_info_mut(d).throw_srcs.retain(|s| !is_not_fn(s, te));
        }
    }
}
