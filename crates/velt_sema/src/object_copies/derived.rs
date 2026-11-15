//! The object literals a copy was spread into (#2956): an error only where one is looked at
//! (module docs of `object_copies`).

use velt_common::{Diagnostic, Span};

use super::{anon, look_words, related, with_notes, Copy, Dynamic, Seen};
use crate::ctx::Ctx;
use crate::hir::{TyId, TyKind};

/// How near a look or a spread is: in another file (or before), then the distance.
type Rank = (bool, u32);

/// Per copy, the literal types it was spread into with the first spread; all of them; and per
/// copy, a spread that is an error by itself, with the field it would overwrite.
type Derived = (
    Vec<Vec<(TyId, Span)>>,
    Vec<TyId>,
    Vec<Option<(Span, String)>>,
);

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
                && spreads(cx, t, c.to)
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

/// A spread into an object literal (or a generic spread at its instantiation): the source part's
/// type, where it is written, the literal's type, and the fields written after it.
pub(super) struct SpreadInto {
    pub part: TyId,
    pub at: Span,
    pub into: TyId,
    pub after: Vec<String>,
}

/// Does a spread of a `t` spread a `target` (also as the payload of `T | undefined` or a member
/// of a union, which a spread copies when it is there)?
pub(super) fn spreads(cx: &mut Ctx, t: TyId, target: TyId) -> bool {
    if related(cx, t, target) {
        return true;
    }
    if let Some(p) = cx.ty.opt_payload(t) {
        if related(cx, p, target) {
            return true;
        }
    }
    let members = cx.union_members(t).unwrap_or_default();
    members.into_iter().any(|m| related(cx, m, target))
}

/// The fields of `c`'s source its copy leaves out: Node's spread copies them too.
fn lost_fields(cx: &mut Ctx, c: &Copy) -> Vec<String> {
    let TyKind::Adt(d, _) = cx.ty.kind(c.from).clone() else {
        return vec![];
    };
    let names: Vec<String> = cx
        .adt(d)
        .map(|a| a.fields.iter().map(|f| f.name.clone()).collect())
        .unwrap_or_default();
    names
        .into_iter()
        .filter(|f| !c.fields.contains(f))
        .collect()
}

/// Does type `t` declare field `f`?
fn declares(cx: &mut Ctx, t: TyId, f: &str) -> bool {
    cx.declared_presence(t, f)
        .is_some_and(|p| p != crate::symbols::Presence::No)
}

/// Per copy, the types of the object literals its converted values were spread into, directly or
/// through further spreads, with the span of the first spread; all of those types; and per copy,
/// a spread that is an error by itself. Node's spread copies the original's fields, so such a
/// literal may lack some (the copy left them out) or hold them in another order: it is an error
/// only where the literal is looked at ([`shows_derived`]), unless its type declares a field the
/// copy left out and nothing written after the spread sets it: Node's spread copies the
/// original's value over the earlier one (`{ b: 9, ...x }.b` reads the original's `b`), so a read
/// shows the copy. Computed to a fixed point, since a copy whose source is one of these types is
/// not [`exempt`].
pub(super) fn derived_targets(
    cx: &mut Ctx,
    copies: &[Copy],
    all_spreads: &[SpreadInto],
) -> Derived {
    let n = copies.len();
    let mut into: Vec<Vec<(TyId, Span)>> = vec![vec![]; n];
    let mut all: Vec<TyId> = vec![];
    let mut refused: Vec<Option<(Span, String)>> = vec![None; n];
    if all_spreads.is_empty() {
        return (into, all, refused);
    }
    loop {
        let mut changed = false;
        for (ci, c) in copies.iter().enumerate() {
            if c.record || refused[ci].is_some() || exempt(cx, c, &all) {
                continue;
            }
            // Each target with the source's fields its objects still lack.
            let lost = lost_fields(cx, c);
            let mut work: Vec<(TyId, Option<Span>, Vec<String>)> = vec![(c.to, None, lost)];
            // A spread that is an error by itself: the one nearest the copy (types, not values,
            // tie them, so every spread of the copy's type counts).
            let mut over: Option<((bool, u32), Span, String)> = None;
            let mut i = 0;
            while i < work.len() {
                let (from, first, lost) = work[i].clone();
                i += 1;
                for s in all_spreads {
                    if cx.dynamic(s.part).is_some() || !spreads(cx, s.part, from) {
                        continue;
                    }
                    let hit =
                        (lost.iter()).find(|f| !s.after.contains(f) && declares(cx, s.into, f));
                    if let Some(f) = hit {
                        let rank = (s.at.file != c.span.file, s.at.lo.abs_diff(c.span.lo));
                        if over.as_ref().is_none_or(|o| rank < o.0) {
                            over = Some((rank, s.at, f.clone()));
                        }
                        continue;
                    }
                    if work.iter().any(|w| w.0 == s.into) {
                        continue;
                    }
                    let at = first.unwrap_or(s.at);
                    // A field the literal declares has its value there (set after the spread).
                    let still: Vec<String> = lost
                        .iter()
                        .filter(|f| !declares(cx, s.into, f))
                        .cloned()
                        .collect();
                    work.push((s.into, Some(at), still));
                    if !into[ci].iter().any(|d| d.0 == s.into) {
                        into[ci].push((s.into, at));
                        changed = true;
                    }
                    if !all.contains(&s.into) {
                        all.push(s.into);
                    }
                }
            }
            if let Some((_, at, f)) = over {
                refused[ci] = Some((at, f));
                changed = true;
            }
        }
        if !changed {
            return (into, all, refused);
        }
    }
}

/// `c`'s copy is spread at `at` into an object whose type declares `field`, which the copy left
/// out and nothing after the spread sets ([`derived_targets`]).
pub(super) fn report_spread_over(cx: &mut Ctx, c: &Copy, at: Span, field: &str) {
    let (from, to) = (cx.display(c.from), cx.display(c.to));
    let d = Diagnostic::error(
        format!("a value of type `{from}` converts to `{to}` by copying its fields, and this program spreads it into an object with a field `{field}`: Node's spread would copy the original's `{field}` over that field"),
        if c.span == Span::DUMMY { at } else { c.span },
    )
    .with_label(at, "spread here, or in the function this calls")
    .with_note(format!("set `{field}` after the spread (`{{ ...x, {field}: … }}`) to give it its own value"));
    let d = with_notes(cx, d, c);
    let d = cx.with_layout_note(d, c.to);
    cx.error(d);
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

impl super::Copies {
    /// Records, per spread in `writes` (an object literal's properties in order: a spread's span,
    /// or none, and the fields each writes), the fields the literal writes after it: a later
    /// property or spread sets them, in Node as here.
    pub(crate) fn written_after(&mut self, writes: &[(Option<Span>, Vec<String>)]) {
        for (i, (at, _)) in writes.iter().enumerate() {
            if let Some(at) = at {
                let after: Vec<String> = writes[i + 1..].iter().flat_map(|w| w.1.clone()).collect();
                self.spread_after.insert(*at, after);
            }
        }
    }
}
