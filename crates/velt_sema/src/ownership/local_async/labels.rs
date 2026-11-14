//! The labels of [`super`]'s errors: where a value leaves its task, and why it gets there.

use crate::ctx::Ctx;
use crate::hir::{TyId, TyKind};

use super::graph::{Boundary, Why};
use super::types;

/// `d` with the label naming where a value leaves its task ([`reaches`]), and, when it crosses
/// because it was made a dynamic value, one at that conversion.
pub(in crate::ownership::local_async) fn with_why(
    cx: &Ctx,
    d: velt_common::Diagnostic,
    w: &Why,
) -> velt_common::Diagnostic {
    let d = d.with_label(w.span, reaches(cx, w));
    match w.made_dyn {
        Some((at, Some(_))) => d.with_label(at, "made a dynamic value here"),
        Some((at, None)) => d.with_label(at, "thrown here, and caught as a dynamic value"),
        None => d,
    }
}

/// The label naming where a non-local closure leaves its task.
pub(in crate::ownership::local_async) fn reaches(cx: &Ctx, w: &Why) -> String {
    let place = match w.boundary {
        // The `spawn` is in a library: the program's call hands the value to it.
        Boundary::Spawn if w.in_library => {
            "is passed here to a function that runs it on other threads"
        }
        Boundary::Spawn => "reaches `spawn` here",
        Boundary::Shared => "reaches `shared(...)` here",
        Boundary::Mutex => "is put in a `Mutex` here, which other threads may lock",
        Boundary::Channel => "is sent on a channel here",
        Boundary::Settle => "settles a promise here, which another task may await",
    };
    // A holder of the closure's own kind (a function type the dynamic value was resolved to) says
    // nothing more than the closure itself.
    let via = w
        .via
        .filter(|&t| w.made_dyn.is_none() || !types::fn_like(cx, t));
    let holder = via.map(|t| {
        let shown = cx.display(t);
        let article = match shown.chars().next() {
            Some(c) if "AEIOUaeiou".contains(c) => "an",
            _ => "a",
        };
        format!("it is stored in {article} `{shown}`")
    });
    // Made dynamic (itself, or the holder it is stored in): a value of a dynamic type crosses at
    // the boundary, and may be this one (#2865).
    if let Some((_, t)) = w.made_dyn {
        let made = match t {
            Some(t) => format!("made a dynamic value (`{}`)", cx.display(t)),
            None => "thrown".to_string(),
        };
        let what = match holder {
            Some(h) => format!("{h}, {made}"),
            None => format!("it is {made}"),
        };
        return format!("but {what}, and a value of a dynamic type {place}");
    }
    match holder {
        Some(h) => format!("but {h}, which {place}"),
        None => format!("but it {place}"),
    }
}

/// The fix for a change to `name` of type `ty` that every call would make to its own copy.
pub(super) fn share_fix(cx: &Ctx, name: &str, ty: TyId) -> String {
    let rest = "or keep what changes in a `shared(...)` or an `Atomic`";
    if name == "this" {
        return format!(
            "share the object the method runs on (create it as `shared(new Mutex(…))` and change it with `.with(…)`), pass what the handler needs as values, {rest}"
        );
    }
    match cx.ty.kind(ty) {
        TyKind::Adt(..) => format!(
            "share the object, `const {name} = shared(new Mutex(new {}(…)))` changed with `{name}.with(…)`, {rest}",
            cx.display(ty)
        ),
        _ => format!(
            "share the value, `const {name} = shared(new Mutex(…))` changed with `{name}.with(…)`, {rest}"
        ),
    }
}
