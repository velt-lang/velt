//! The errors of a copy the program can tell from the value it was made of: a write to it, or a
//! look that sees its identity, with the notes that name the conversion.

use velt_common::{Diagnostic, Span};

use super::{Copy, Seen, Write};
use crate::ctx::Ctx;
use crate::hir::TyId;

pub(super) fn report_write(cx: &mut Ctx, c: &Copy, w: &Write) {
    let (from, to) = (cx.display(c.from), cx.display(c.to));
    let (field, label) = if w.name.is_empty() {
        (
            "a field through `any`".to_string(),
            "a field is assigned here".to_string(),
        )
    } else {
        let name = cx.shown_name(&w.name);
        (
            format!("field `{name}`"),
            format!("`{name}` is assigned here"),
        )
    };
    // A copy an interface's getter makes of an object converted to it (`object_assigns.rs`)
    // has no place in the source: the write is the place, reported once (a part write goes
    // through the getter's copy and the setter's).
    const GETTER_COPY: &str = "an object converted to an interface holds";
    let reported = |cx: &Ctx| {
        cx.diags.iter().any(|d| {
            d.message.starts_with(GETTER_COPY) && d.labels.first().is_some_and(|l| l.span == w.span)
        })
    };
    if c.span == Span::DUMMY && reported(cx) {
        return;
    }
    let d = match c.span == Span::DUMMY {
        true => Diagnostic::error(
            format!(
                "{GETTER_COPY} a `{from}` that the interface reads as `{to}` by copying its fields, and this assigns {field} of that copy, which the object would not see"
            ),
            w.span,
        ),
        false => Diagnostic::error(
            format!(
                "a value of type `{from}` converts to `{to}` by copying its fields, and this program assigns {field}, which the other object would not see"
            ),
            c.span,
        )
        .with_label(w.span, label),
    };
    let d = match c.span == Span::DUMMY {
        true => d
            .with_note(format!("Node changes the object's own field; Velt's object types have fixed layouts, so each read of the field through the interface is a new `{to}` with the same field values"))
            .with_note(format!("give the object's field the interface's type (`{to}`), or assign the whole field through the interface")),
        false => with_notes(cx, d, c),
    };
    let d = cx.with_layout_note(d, c.to);
    cx.error(d);
}

pub(super) fn report_seen(
    cx: &mut Ctx,
    c: &Copy,
    t: TyId,
    span: Span,
    how: Seen,
    in_keys: &[String],
) {
    let (from, to) = (cx.display(c.from), cx.display(c.to));
    let (what, why) = look_words(cx, t, how, in_keys);
    let d = Diagnostic::error(
        format!("a value of type `{from}` converts to `{to}` by copying its fields, and this program {what}: {why}"),
        // A copy an interface's getter makes (`object_assigns.rs`) has no place of its own.
        if c.span == Span::DUMMY { span } else { c.span },
    )
    .with_label(span, "here, or in the function this calls");
    let d = with_notes(cx, d, c);
    let d = cx.with_layout_note(d, c.to);
    cx.error(d);
}

/// How the look `how` at a `t` reads in a diagnostic, and why it shows a copy.
pub(super) fn look_words(
    cx: &mut Ctx,
    t: TyId,
    how: Seen,
    in_keys: &[String],
) -> (String, &'static str) {
    let shown = cx.display(t);
    // `an` before a vowel sound: `an any`, `an unknown`, `a User`.
    let a = match shown.starts_with(['A', 'E', 'I', 'O', 'a', 'e', 'i', 'o'])
        || shown.starts_with("un")
    {
        true => "an",
        false => "a",
    };
    let what = match how {
        Seen::Printed => format!("prints {a} `{shown}`"),
        Seen::Serialized => format!("serializes {a} `{shown}`"),
        Seen::Keys => format!("lists the keys of {a} `{shown}`"),
        Seen::Identity => format!("compares `{shown}` values with `===` or `!==`"),
        Seen::Key => format!("uses `{shown}` values as keys compared by identity"),
        // Never reported itself (`sees`).
        Seen::FromDynamic => format!("converts a dynamic value to {a} `{shown}`"),
        Seen::Dynamic(_) => format!("converts {a} `{shown}` to a dynamic type"),
        Seen::Spread(_) => format!("spreads {a} `{shown}`"),
        Seen::In(i) => format!(
            "tests {a} `{shown}` for \"{}\" with `in`",
            in_keys[i as usize]
        ),
    };
    let why = match how {
        Seen::Identity => "two copies of one object are not `===`",
        Seen::Key => "two copies of one object are different keys",
        Seen::Spread(_) => "Node would copy all the original object's fields, in its order",
        Seen::In(_) => "the copy lacks the original's other fields, which Node would find",
        _ => "Node would show the original object's fields, in its order",
    };
    (what, why)
}

pub(super) fn with_notes(cx: &mut Ctx, d: Diagnostic, c: &Copy) -> Diagnostic {
    let (from, to) = (cx.display(c.from), cx.display(c.to));
    let src = c.source.as_deref().unwrap_or("x");
    // A symbol key as written: `[KEY]: x[KEY]`.
    let fields: Vec<String> = (c.fields.iter())
        .map(|f| match cx.key_spelling(f) {
            Some(k) => format!("{k}: {src}{k}"),
            None => format!("{f}: {src}.{f}"),
        })
        .collect();
    let fields = match c.source {
        Some(_) => format!("`{{ {} }}`", fields.join(", ")),
        // An element of a converted array, or another value without a name.
        None => format!("`(x) => ({{ {} }})` with `.map`", fields.join(", ")),
    };
    d.with_note(format!(
        "TypeScript allows this and passes the same object; Velt's object types have fixed layouts, so the `{to}` made from a `{from}` value is a new object with the same field values"
    ))
    .with_note(format!(
        "build the new object explicitly, {fields} (a new object in TypeScript too), or declare the parameter or variable as `{from}`"
    ))
}
