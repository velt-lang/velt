//! Record casts and the record copies `FnCx::widen_record_copy` makes (`Copy::record`): a
//! record holding a copy of an object's fields, or of another record's entries with their values
//! converted (`Record<string, string | undefined>` as a `Record<string, string | boolean |
//! undefined>`). The copy is an error where the program could tell it from the original: a write
//! to a record of the copy's type, or, for a record source, of the source's type, which the other
//! would not see; or an identity test of the copy's type (two copies of one record are not `===`).

use rustc_hash::FxHashSet;

use velt_common::{Diagnostic, Span};

use super::record_flow::Lives;
use super::{
    compared, dynamic_reaches, holds, related, report_seen, Copy, Dynamic, Seen, Write,
};
use crate::copy_flow::Store;
use crate::ctx::Ctx;
use crate::hir::{DefId, TyId, TyKind};

/// What runs while a copy lives (`record_flow`), and the stores its value reaches.
type Live = (FxHashSet<DefId>, FxHashSet<Store>);

/// The program's record writes, its other writes and its looks at values (`object_copies`).
pub(super) type Observed<'a> = (&'a [(TyId, Span)], &'a [Write], &'a [(TyId, Span, Seen)]);

/// The lifetimes of the record copies among `copies` (`spans`: their spans and types), `None`
/// where there is none: the program's writes and looks are found by their spans.
pub(super) fn lives(
    cx: &mut Ctx,
    (copies, spans): (&[Copy], &[(Span, TyId)]),
    record_writes: &[(TyId, Span)],
    writes: &[Write],
    seen: &[(TyId, Span, Seen)],
) -> Option<Lives> {
    if !copies.iter().any(|c| c.record) {
        return None;
    }
    let at: FxHashSet<Span> = (writes.iter().map(|w| w.span))
        .chain(seen.iter().map(|s| s.1))
        .collect();
    let recorded: FxHashSet<Span> = record_writes.iter().map(|w| w.1).collect();
    Some(Lives::build(cx, spans, &at, &recorded))
}

/// Is the record cast or record copy `ci` an error, reported here: a write or an identity test
/// that tells it from its source?
pub(super) fn refused(
    cx: &mut Ctx,
    (c, ci): (&Copy, usize),
    (record_writes, writes, seen): Observed<'_>,
    in_keys: &[String],
    looks: &Dynamic,
    lives: Option<&Lives>,
) -> bool {
    let of_record = of_record(cx, c);
    // While the copy lives, where that is known (`record_flow`).
    // (A record cast's writes stay type-level: its source is an object, whose fields the general
    // rules check; only its identity tests are limited to its lifetime.)
    let live = lives.and_then(|l| l.live(ci).map(|v| (l, v)));
    let during = |span: Span| match &live {
        Some((l, (fns, _))) => l.function_at(span).is_none_or(|f| fns.contains(&f)),
        None => true,
    };
    let written = match of_record {
        true => written(cx, (c, ci), record_writes, writes, looks, live.as_ref()),
        false => (record_writes.iter())
            .find(|w| related_base(cx, w.0, c.to))
            .map(|w| w.1),
    };
    if let Some(at) = written {
        report_write(cx, c, at);
        return true;
    }
    // Identity: the record is another object, which a dynamic value may hold.
    let shown = seen.iter().find(|s| {
        matches!(s.2, Seen::Identity | Seen::Key)
            && during(s.1)
            && compared(cx, s.0, &mut FxHashSet::default())
                .into_iter()
                .any(|m| cx.dynamic(m).is_some() || related_base(cx, m, c.to))
    });
    match shown {
        Some(&(t, span, how)) if of_record => report_identity(cx, c, t, span, how),
        Some(&(t, span, how)) => report_seen(cx, c, t, span, how, in_keys),
        None => return false,
    }
    true
}

/// Is `c` a copy of a record's entries (not an object's fields)?
pub(super) fn of_record(cx: &Ctx, c: &Copy) -> bool {
    c.record && is_record(cx, c.from)
}

fn is_record(cx: &Ctx, t: TyId) -> bool {
    matches!(cx.ty.kind(t), TyKind::Adt(d, _) if Some(*d) == cx.prelude_adt("Record"))
}

/// `t`, or the record it brands when it is a record view (written and compared as its record).
fn base(cx: &Ctx, t: TyId) -> TyId {
    cx.record_view(t).map_or(t, |(b, _)| b)
}

/// Is a `t` (a record view seen as its record) one record type with `of`?
pub(super) fn related_base(cx: &mut Ctx, t: TyId, of: TyId) -> bool {
    let t = base(cx, t);
    related(cx, t, of)
}

/// Where the program writes to a record the copy `c` (of a record) would differ from: one of its
/// type or of the source's (`r[k] = v`, `delete r[k]`, `Object.assign`), or one held in a dynamic
/// value (`a[k] = v`, `a.k = v` with `a: any`), once the program has made either one dynamic.
/// With `live` (`refused`), only writes made while the copy lives count, and a typed write only
/// if its record may be the copy or the source.
fn written(
    cx: &mut Ctx,
    (c, ci): (&Copy, usize),
    record_writes: &[(TyId, Span)],
    writes: &[Write],
    looks: &Dynamic,
    live: Option<&(&Lives, Live)>,
) -> Option<Span> {
    let hit = |cx: &mut Ctx, t: TyId| {
        // A write in generic code (`Record<string, V>`) may write any record of that shape.
        let generic = cx.has_type_params(t) && is_record(cx, base(cx, t));
        generic || related_base(cx, t, c.to) || related_base(cx, t, c.from)
    };
    let typed = |&(t, at): &(TyId, Span), cx: &mut Ctx| {
        hit(cx, t) && live.is_none_or(|(l, (fns, reach))| l.may_write(ci, at, fns, reach))
    };
    if let Some(&(_, at)) = record_writes.iter().find(|w| typed(w, cx)) {
        return Some(at);
    }
    let during = |span: Span| match live {
        Some((l, (fns, _))) => l.function_at(span).is_none_or(|f| fns.contains(&f)),
        None => true,
    };
    let through_any = (writes.iter()).find(|w| w.dynamic && w.ty.is_none() && during(w.span))?;
    // The copy itself, as any value of its type the program makes dynamic (by type, where the
    // copy flow may not see where the copy goes).
    let copy_dynamic = looks.seen.iter().any(|&(t, _, how)| {
        matches!(how, Seen::Dynamic(_)) && holds(cx, t, c.to, &mut FxHashSet::default())
    });
    (copy_dynamic || dynamic_reaches(cx, (c, ci), looks)).then_some(through_any.span)
}

/// A record cast or record copy `c` in a program that writes to such a record at `at`.
pub(super) fn report_write(cx: &mut Ctx, c: &Copy, at: Span) {
    let to = cx.display(c.to);
    if !of_record(cx, c) {
        let from = cx.display(c.from);
        cx.error(
            Diagnostic::error(
                format!("a value of type `{from}` converts to `{to}` by copying its fields, and this program writes to such a record, which the object would not see"),
                c.span,
            )
            .with_label(at, "written here")
            .with_note("build the record with the fields you need (`{ a: x.a }`)"),
        );
        return;
    }
    let from = c.source.clone().unwrap_or_else(|| cx.display(c.from));
    let d = Diagnostic::error(
        format!("a `{from}` converts to `{to}` by copying its entries, and this program writes to a record of one of the two types, which the other would not see"),
        c.span,
    )
    .with_label(at, "written here");
    cx.error(notes(d, &from, &to));
}

/// The record copy `c` in a program that compares or keys `t` values by identity at `span`.
pub(super) fn report_identity(cx: &mut Ctx, c: &Copy, t: TyId, span: Span, how: Seen) {
    let (to, shown) = (cx.display(c.to), cx.display(t));
    let from = c.source.clone().unwrap_or_else(|| cx.display(c.from));
    let what = match how {
        Seen::Key => format!("uses `{shown}` values as keys compared by identity"),
        _ => format!("compares `{shown}` values with `===` or `!==`"),
    };
    let d = Diagnostic::error(
        format!("a `{from}` converts to `{to}` by copying its entries, and this program {what}: two copies of one record are not `===`"),
        c.span,
    )
    .with_label(span, "here, or in the function this calls");
    cx.error(notes(d, &from, &to));
}

fn notes(d: Diagnostic, from: &str, to: &str) -> Diagnostic {
    d.with_note(format!(
        "TypeScript allows this and passes the same record; a `{to}` holds its values in another layout than a `{from}`, so the `{to}` is a new record with the same entries"
    ))
    .with_note(format!(
        "pass a copy, `{{ ...x }}` (a new record in TypeScript too), or declare the parameter or variable as `{from}`"
    ))
}
