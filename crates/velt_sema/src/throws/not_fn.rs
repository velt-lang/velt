//! Node's `TypeError` for a call through `any` that holds no function (`o.ob()` with `o.ob`
//! a number), where no `catch` of the calling function encloses the call: the error leaves the
//! function, as in Node. An async function's promise rejects with it, and a sync function's
//! caller can catch it (velt_vir any_not_fn.rs throws it to the function's own error).
//!
//! A function whose error type is open (inferred from its body) counts it among what it throws
//! ([`FnCx::note_any_call`](crate::body::FnCx) records the source); one whose error type is
//! written (`throws E`), fixed by the function type its closure is checked against, or bound
//! by the interface method it implements does not: the source is left out here, before
//! inference, so no clause or bound that held before is broken, and velt_vir stops the program
//! with the message there, as it did before.

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
            || groups.group_of(d).is_some_and(|g| groups.list[g].bound.is_some());
        if fixed && info.throw_srcs.iter().any(|s| is_not_fn(s, te)) {
            cx.fn_info_mut(d).throw_srcs.retain(|s| !is_not_fn(s, te));
        }
    }
}
