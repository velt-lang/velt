//! The object literals a copy was spread into (#2956): an error only where one is looked at
//! (module docs of `object_copies`).

use velt_common::{Diagnostic, Span};

use super::{anon, look_words, related, with_notes, Copy, Dynamic, Seen};
use crate::ctx::Ctx;
use crate::hir::TyId;

/// How near a look or a spread is: in another file (or before), then the distance.
type Rank = (bool, u32);

/// Per copy, the literal types it was spread into with the first spread; and all of them.
type Derived = (Vec<Vec<(TyId, Span)>>, Vec<TyId>);

/// The best look found so far: its rank, the literal's type and the spread, and the look.
type Found = (Rank, (TyId, Span), (TyId, Span, Seen));

/// Report each copy (not already reported, `flagged`) whose object literal it was spread into
/// (`spread_into`, [`derived_targets`]) the program looks at.
pub(super) fn report_looks(
    cx: &mut Ctx,
    copies: &[Copy],
    flagged: &[bool],
    spread_into: &[Vec<(TyId, Span)>],
    seen: &[(TyId, Span, Seen)],
    in_keys: &[String],
    looks: &Dynamic,
) {
    for (ci, c) in copies.iter().enumerate() {
        if flagged[ci] {
            continue;
        }
        if spread_into[ci].is_empty() {
            continue;
        }
        // Types, not values, tie a copy to a spread and a spread to a look, and anonymous types
        // are shared by every literal of their shape: name the spread nearest the copy and the
        // look nearest after (or around) that spread, in the same file.
        let near = |from: Span, to: Span| {
            let after = to.file == from.file && to.hi >= from.lo;
            (
                !after,
                if after {
                    to.lo.saturating_sub(from.lo)
                } else {
                    0
                },
            )
        };
        // The spread: nearest to the copy on either side (a helper or a constructor that spreads
        // may come first).
        let dist = |from: Span, to: Span| (to.file != from.file, to.lo.abs_diff(from.lo));
        let mut at = spread_into[ci][0].1;
        let mut at_rank = (true, u32::MAX);
        for &(t, span, how) in seen {
            if matches!(how, Seen::Spread(Some(_)))
                && cx.dynamic(t).is_none()
                && related(cx, t, c.to)
                && dist(c.span, span) < at_rank
            {
                (at, at_rank) = (span, dist(c.span, span));
            }
        }
        let mut best: Option<Found> = None;
        for &(r, _) in &spread_into[ci] {
            for &(t, span, how) in seen {
                if !shows_derived(cx, c, r, (t, how), in_keys, looks) {
                    continue;
                }
                let rank = near(at, span);
                if best.as_ref().is_none_or(|b| rank < b.0) {
                    best = Some((rank, (r, at), (t, span, how)));
                }
            }
        }
        if let Some((_, into, look)) = best {
            report_derived(cx, c, into, look, in_keys);
        }
    }
}

/// `c`'s copy was spread into an object literal of type `r` at `at`, and the program looks at
/// that object (`t` at `span`, [`shows_derived`]).
fn report_derived(
    cx: &mut Ctx,
    c: &Copy,
    (r, at): (TyId, Span),
    (t, span, how): (TyId, Span, Seen),
    in_keys: &[String],
) {
    let (from, to, into) = (cx.display(c.from), cx.display(c.to), cx.display(r));
    let (what, _) = look_words(cx, t, how, in_keys);
    let d = Diagnostic::error(
        format!("a value of type `{from}` converts to `{to}` by copying its fields, it is spread into a `{into}`, and this program {what}: Node's spread copies all the original object's fields, in its order, so that object would show them"),
        if c.span == Span::DUMMY { span } else { c.span },
    )
    .with_label(at, "spread here")
    .with_label(span, "and looked at here, or in the function this calls");
    let d = with_notes(cx, d, c);
    let d = cx.with_layout_note(d, c.to);
    cx.error(d);
}

/// Does `c`'s copy show what the original would wherever it is spread, printed, serialized or
/// its keys listed? It has the source's fields in order (`Copy::in_order`), of a fresh source or
/// of an anonymous object type's value, which has exactly its type's fields, unless the source
/// type is one a copy was spread into (`derived`): such a value lacks fields Node's object has.
pub(super) fn exempt(cx: &mut Ctx, c: &Copy, derived: &[TyId]) -> bool {
    c.in_order && (c.fresh || anon(cx, c.from)) && !derived.iter().any(|&r| related(cx, c.from, r))
}

/// Per copy, the types of the object literals its converted values were spread into, directly or
/// through further spreads (`Seen::Spread(Some(_))`), with the span of the first spread; and all
/// of those types. Node's spread copies the original's fields, so such a literal may lack some
/// (the copy left them out) or hold them in another order: it is an error only where the literal
/// is looked at ([`shows_derived`]). Computed to a fixed point, since a copy whose source is one
/// of these types is not [`exempt`].
pub(super) fn derived_targets(
    cx: &mut Ctx,
    copies: &[Copy],
    seen: &[(TyId, Span, Seen)],
) -> Derived {
    let mut into: Vec<Vec<(TyId, Span)>> = vec![vec![]; copies.len()];
    let mut all: Vec<TyId> = vec![];
    if !seen.iter().any(|s| matches!(s.2, Seen::Spread(Some(_)))) {
        return (into, all);
    }
    loop {
        let mut changed = false;
        for (ci, c) in copies.iter().enumerate() {
            if c.record || exempt(cx, c, &all) {
                continue;
            }
            let mut work: Vec<(TyId, Option<Span>)> = vec![(c.to, None)];
            let mut i = 0;
            while i < work.len() {
                let (from, first) = work[i];
                i += 1;
                for &(t, at, how) in seen {
                    let Seen::Spread(Some(r)) = how else {
                        continue;
                    };
                    if cx.dynamic(t).is_some() || !related(cx, t, from) {
                        continue;
                    }
                    if work.iter().any(|w| w.0 == r) {
                        continue;
                    }
                    let at = first.unwrap_or(at);
                    work.push((r, Some(at)));
                    if !into[ci].iter().any(|d| d.0 == r) {
                        into[ci].push((r, at));
                        changed = true;
                    }
                    if !all.contains(&r) {
                        all.push(r);
                    }
                }
            }
        }
        if !changed {
            return (into, all);
        }
    }
}

/// Does the look `s` show the object literal of type `r` that `c`'s copy was spread into
/// ([`derived_targets`])? Printing, serializing or listing the keys of a value holding one, an
/// `in` test of one for a key the original has, converting a value holding one to a dynamic type,
/// or spreading one where the result's type is not known (generic code, JSX props).
fn shows_derived(
    cx: &mut Ctx,
    c: &Copy,
    r: TyId,
    (t, how): (TyId, Seen),
    in_keys: &[String],
    looks: &Dynamic,
) -> bool {
    match how {
        Seen::Printed | Seen::Serialized | Seen::Keys | Seen::Dynamic(_) => looks.holds(cx, t, r),
        Seen::In(i) => {
            let k = &in_keys[i as usize];
            related(cx, t, r)
                && cx
                    .declared_presence(c.from, k)
                    .is_some_and(|p| p != crate::symbols::Presence::No)
        }
        Seen::Spread(None) => related(cx, t, r),
        Seen::Spread(Some(_)) | Seen::Identity | Seen::Key | Seen::FromDynamic => false,
    }
}
