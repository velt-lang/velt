//! The errors of a closure that is not local: the captures it modifies, and the generators and
//! atomics it captured, each naming the boundary the closure reaches.

use velt_common::Diagnostic;

use super::graph::{Boundary, Why};
use super::with_why;
use crate::ctx::Ctx;
use crate::hir::{Def, DefId, TyId, TyKind};

/// A non-local async closure copies what it captured for each call (and a plain closure's
/// promise carries what the closure passed on), so a captured value holding an `Atomic` (outside
/// a `shared`) would be changed in a copy the task that created it never sees: report it. A closure literal spawned where it is written is reported by the boundary
/// check instead, which names the variable used again (`boundary.rs`).
pub(super) fn report_atomics(cx: &mut Ctx, c: DefId, why: Option<Why>, plain: bool) {
    let closure = cx.def_spans.get(c.0 as usize).copied();
    if let (Some(at), Some(w)) = (closure, why) {
        if w.boundary == Boundary::Spawn
            && at.file == w.span.file
            && w.span.lo <= at.lo
            && at.hi <= w.span.hi
        {
            return;
        }
    }
    let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
        return;
    };
    let caps: Vec<(TyId, String, velt_common::Span)> = f
        .captures
        .iter()
        .map(|cap| &f.body.locals[cap.inner.0 as usize])
        .map(|l| (l.ty, l.name.clone(), l.span))
        .collect();
    for (ty, name, at) in caps {
        if cx.atomic_value(ty).is_some() || !cx.holds_atomic(ty) {
            continue;
        }
        let tn = cx.display(ty);
        let mut d = Diagnostic::error(
            match plain {
                true => format!("a closure whose promise may run on another thread captures `{name}`, whose `{tn}` holds an `Atomic`"),
                false => format!("an async closure that may run on another thread captures `{name}`, whose `{tn}` holds an `Atomic`"),
            },
            at,
        );
        if let Some(w) = why {
            d = with_why(cx, d, &w);
        }
        cx.error(d.with_note(format!(
            "{}, and changes to the copy's atomics would not reach `{name}`: share it so every task changes the same atomics: `const {name} = shared(new {tn}(…))`",
            match plain {
                true => "the promise it returns goes to the other task with a copy of what the closure passed on",
                false => "such a closure copies what it captured for each call",
            }
        )));
    }
}

/// A non-local async closure copies what it captured for each call, and a generator's state
/// cannot be copied: report each captured generator (a local one shares it, as in JS).
pub(super) fn report_generators(cx: &mut Ctx, c: DefId, why: Option<Why>) {
    let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
        return;
    };
    let caps: Vec<(TyId, String, velt_common::Span)> = f
        .captures
        .iter()
        .map(|cap| &f.body.locals[cap.inner.0 as usize])
        .map(|l| (l.ty, l.name.clone(), l.span))
        .collect();
    let gens: Vec<_> = caps
        .into_iter()
        .filter(|c| cx.holds_generator(c.0))
        .collect();
    for (_, name, at) in gens {
        let mut d = Diagnostic::error(
            format!("an async closure that may run on another thread cannot capture the generator `{name}`"),
            at,
        );
        if let Some(w) = why {
            d = with_why(cx, d, &w);
        }
        cx.error(d.with_note("such a closure copies what it captured for each call, and a generator's suspended state cannot be copied: create the generator inside the closure, or keep the closure on the task that created it"));
    }
}

/// "this `what` modifies captured `x`" for each capture closure `c` modifies, naming the
/// boundary `why` it reaches.
pub(super) fn report_mutated(cx: &mut Ctx, c: DefId, why: Option<Why>, what: &str) {
    let mutated = std::mem::take(&mut cx.fn_info_mut(c).mutated_captures);
    let closure = cx.def_spans.get(c.0 as usize).copied();
    for (name, at) in mutated {
        // Also through the closures that call `c` (`mutated_captures`): one change, one error.
        if cx.reported_mutations.contains(&(name.clone(), at)) {
            continue;
        }
        cx.reported_mutations.push((name.clone(), at));
        // The capture is then moved into the closure: no second error for a later use of it.
        if let Some(span) = closure {
            cx.reported_captures.push((name.clone(), span));
        }
        let mut d = Diagnostic::error(
            format!("this {what} modifies captured `{name}`, so it must stay on the task that created it"),
            at,
        );
        if let Some(w) = why {
            d = with_why(cx, d, &w);
        }
        let runs = match why.map(|w| w.boundary) {
            Some(Boundary::Spawn) => "the spawned task runs it on another thread",
            Some(Boundary::Shared | Boundary::Mutex) => {
                "every thread holding the `shared` value may call it"
            }
            Some(Boundary::Channel) => "the task receiving it runs it on another thread",
            Some(Boundary::Settle) => "the task awaiting the promise may run it on another thread",
            None => "another thread may run it",
        };
        // `shared` holds a number (`add`) or any value (`get`/`set`); a collection or an object
        // is changed in place, which needs a `Mutex`.
        let share = match capture_ty(cx, c, &name).map(|t| cx.ty.kind(t).clone()) {
            Some(TyKind::Int(_) | TyKind::Float(_)) => format!(
                "share it with `shared`: `const {name} = shared(...)` and `{name}.add(1)` / `{name}.set(v)`"
            ),
            Some(TyKind::Str | TyKind::Bool | TyKind::Literal(_)) => format!(
                "share it with `shared`: `const {name} = shared(...)` and `{name}.get()` / `{name}.set(v)`"
            ),
            _ => format!(
                "share it in a `Mutex`: `const {name} = shared(new Mutex(...))`, changed with `{name}.with(...)`"
            ),
        };
        cx.error(d.with_note(format!(
            "{runs}, with its own copy of what it captured: pass `{name}` as a parameter instead, or {share}"
        )));
    }
}

/// The type closure `c` captured the variable `name` with.
fn capture_ty(cx: &Ctx, c: DefId, name: &str) -> Option<TyId> {
    let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
        return None;
    };
    f.captures
        .iter()
        .map(|cap| &f.body.locals[cap.inner.0 as usize])
        .find(|l| l.name == name)
        .map(|l| l.ty)
}
