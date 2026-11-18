//! Record casts and the record copies `FnCx::widen_record_copy` makes (`Copy::record`): a
//! record holding a copy of an object's fields, or of another record's entries with their values
//! converted (`Record<string, string | undefined>` as a `Record<string, string | boolean |
//! undefined>`). The copy is an error where the program could tell it from the original: a write
//! to a record of the copy's type, or, for a record source, of the source's type, which the other
//! would not see; or an identity test of the copy's type (two copies of one record are not `===`).

use rustc_hash::FxHashSet;

use velt_common::{Diagnostic, Span};

use super::{dynamic_reaches, holds, related, Copy, Dynamic, Seen, Write};
use crate::ctx::Ctx;
use crate::hir::{TyId, TyKind};

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
pub(super) fn written(
    cx: &mut Ctx,
    (c, ci): (&Copy, usize),
    record_writes: &[(TyId, Span)],
    writes: &[Write],
    looks: &Dynamic,
) -> Option<Span> {
    let hit = |cx: &mut Ctx, t: TyId| {
        // A write in generic code (`Record<string, V>`) may write any record of that shape.
        let generic = cx.has_type_params(t) && is_record(cx, base(cx, t));
        generic || related_base(cx, t, c.to) || related_base(cx, t, c.from)
    };
    if let Some(&(_, at)) = record_writes.iter().find(|w| hit(cx, w.0)) {
        return Some(at);
    }
    let through_any = writes.iter().find(|w| w.dynamic && w.ty.is_none())?;
    // The copy itself, as any value of its type the program makes dynamic (the copy flow doesn't
    // follow a record copy, so it can't rule one out).
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
