//! Object values converted by copying their fields (`body::expr::object_copy`): an object type
//! where another object type with the same fields in another order (#651), or with only some
//! of its fields (#650), is expected. TypeScript passes the same object; Velt's object types have
//! fixed layouts, so the converted value is a new object holding the same field values (nested
//! objects are shared, not copied). Once the program is checked, a conversion is an error where
//! the program could tell the copy from the original:
//!
//! - it assigns one of the copied fields on a value of either type (one object would miss it);
//! - it compares values of the converted-to type with `===` (two copies of one object are not
//!   `===`);
//! - it prints, serializes (`JSON.stringify`) or lists the keys (`Object.keys`) of a value whose
//!   type holds the converted-to type (Node shows the original's keys, in its order).
//!
//! The fix keeps Node's behavior: an object literal with the fields (`{ a: x.a }`) is a new
//! object in TypeScript too. Generic code is checked where it has concrete types (`JSON.stringify` everywhere).

use std::collections::HashSet;

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::hir::{TyId, TyKind};

/// One conversion that copied the fields `fields` of a `from` value into a new `to`.
pub(crate) struct Copy {
    pub from: TyId,
    pub to: TyId,
    pub fields: Vec<String>,
    pub span: Span,
    /// The source as written (`ab`, `p.owner`), for the fix.
    pub source: Option<String>,
    /// The source is a literal nothing else refers to: only the copy's shape can show.
    pub fresh: bool,
}

/// An assignment to field `name` of a value of type `ty` (`None`: a type parameter's, which may
/// be any object type).
pub(crate) struct Write {
    pub ty: Option<TyId>,
    pub name: String,
    pub span: Span,
}

/// How a program looks at a value (other than reading its fields).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Seen {
    /// `console.log`.
    Printed,
    /// `JSON.stringify`.
    Serialized,
    /// `Object.keys`.
    Keys,
    /// `===` / `!==`.
    Identity,
}

/// The conversions, field writes and looks seen so far.
#[derive(Default)]
pub(crate) struct Copies {
    pub copies: Vec<Copy>,
    pub writes: Vec<Write>,
    seen: Vec<(TyId, Span, Seen)>,
}

impl Copies {
    /// How many of each there are now (`body::recursion::Mark`).
    pub(crate) fn mark(&self) -> [usize; 3] {
        [self.copies.len(), self.writes.len(), self.seen.len()]
    }

    /// Drop what was added since `mark`.
    pub(crate) fn rollback(&mut self, mark: [usize; 3]) {
        self.copies.truncate(mark[0]);
        self.writes.truncate(mark[1]);
        self.seen.truncate(mark[2]);
    }

    /// The program looks at a value of type `t` at `span`.
    pub(crate) fn observe(&mut self, t: TyId, span: Span, how: Seen) {
        self.seen.push((t, span, how));
    }
}

/// Report the conversions whose copy the program could tell from the original (module docs).
pub(crate) fn check(cx: &mut Ctx) {
    let Copies {
        copies,
        writes,
        seen,
    } = std::mem::take(&mut cx.object_copies);
    for c in &copies {
        if let Some(w) = assigned(cx, c, &writes) {
            report_write(cx, c, w);
        } else if let Some(&(t, span, how)) = seen.iter().find(|s| sees(cx, c, s.0, s.2)) {
            report_seen(cx, c, t, span, how);
        }
    }
}

/// An assignment to a copied field of either type of `c`.
fn assigned<'w>(cx: &mut Ctx, c: &Copy, writes: &'w [Write]) -> Option<&'w Write> {
    if c.fresh {
        return None;
    }
    writes.iter().find(|w| {
        c.fields.contains(&w.name)
            && w.ty
                .is_none_or(|t| related(cx, t, c.from) || related(cx, t, c.to))
    })
}

/// Does looking at a `t` (`how`) show that `c` made a copy?
fn sees(cx: &mut Ctx, c: &Copy, t: TyId, how: Seen) -> bool {
    match how {
        Seen::Identity => !c.fresh && related(cx, t, c.to),
        _ => holds(cx, t, c.to, &mut HashSet::new()),
    }
}

/// Are `a` and `b` one object type, as conversions see it?
fn related(cx: &mut Ctx, a: TyId, b: TyId) -> bool {
    a == b || cx.canon(a) == cx.canon(b) || cx.same_layout(a, b)
}

/// Can a value of type `t` hold a `target` (itself, an element, a field, …)?
fn holds(cx: &mut Ctx, t: TyId, target: TyId, seen: &mut HashSet<TyId>) -> bool {
    if !seen.insert(t) {
        return false;
    }
    if related(cx, t, target) {
        return true;
    }
    let kind = cx.ty.kind(t).clone();
    let mut parts = crate::types::children(&kind);
    if let Some(members) = cx.union_members(t) {
        parts.extend(members);
    }
    if let TyKind::Adt(d, args) = kind {
        let fields: Vec<TyId> = cx
            .adt(d)
            .map(|a| a.fields.iter().map(|f| f.ty).collect())
            .unwrap_or_default();
        parts.extend(fields.into_iter().map(|f| cx.subst(f, &args)));
    }
    parts.into_iter().any(|p| holds(cx, p, target, seen))
}

fn report_write(cx: &mut Ctx, c: &Copy, w: &Write) {
    let (from, to) = (cx.display(c.from), cx.display(c.to));
    let d = Diagnostic::error(
        format!(
            "a value of type `{from}` converts to `{to}` by copying its fields, and this program assigns field `{}`, which the other object would not see",
            w.name
        ),
        c.span,
    )
    .with_label(w.span, format!("`{}` is assigned here", w.name));
    let d = with_notes(cx, d, c);
    cx.error(d);
}

fn report_seen(cx: &mut Ctx, c: &Copy, t: TyId, span: Span, how: Seen) {
    let (from, to, shown) = (cx.display(c.from), cx.display(c.to), cx.display(t));
    let what = match how {
        Seen::Printed => format!("prints a `{shown}`"),
        Seen::Serialized => format!("serializes a `{shown}`"),
        Seen::Keys => format!("lists the keys of a `{shown}`"),
        Seen::Identity => format!("compares `{shown}` values with `===`"),
    };
    let why = match how {
        Seen::Identity => "two copies of one object are not `===`",
        _ => "Node would show the original object's fields, in its order",
    };
    let d = Diagnostic::error(
        format!("a value of type `{from}` converts to `{to}` by copying its fields, and this program {what}: {why}"),
        c.span,
    )
    .with_label(span, "here");
    let d = with_notes(cx, d, c);
    cx.error(d);
}

fn with_notes(cx: &mut Ctx, d: Diagnostic, c: &Copy) -> Diagnostic {
    let (from, to) = (cx.display(c.from), cx.display(c.to));
    let src = c.source.as_deref().unwrap_or("x");
    let fields: Vec<String> = c.fields.iter().map(|f| format!("{f}: {src}.{f}")).collect();
    d.with_note(format!(
        "TypeScript allows this and passes the same object; Velt's object types have fixed layouts, so the `{to}` made from a `{from}` value is a new object with the same field values"
    ))
    .with_note(format!(
        "build the new object explicitly, `{{ {} }}` (a new object in TypeScript too), or declare the parameter or variable as `{from}`",
        fields.join(", ")
    ))
}
