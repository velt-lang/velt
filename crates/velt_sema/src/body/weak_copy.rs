//! The errors for a value that holds a weak collection (`WeakMap`, `WeakSet`, `WeakRef`) or a
//! class with a `weak` field crossing to another task, a lock or a copy
//! (docs/internals/design/weak-refs.md "Threads and `shared`", cycles.md "`weak T`"): both
//! belong to the task that made them. The checks where those are written and
//! `crate::weak_threads`, which finds the hidden ones, report them with these.

use velt_common::{Diagnostic, Span};

use super::{FnCx, GenCopy};
use crate::hir::{TyId, TyKind};

impl FnCx<'_, '_> {
    /// Report `what` copying the value of type `t`, which holds a weak collection or a `weak`
    /// field, at `span`.
    pub(crate) fn no_weak_copy(&mut self, t: TyId, what: GenCopy, span: Span) {
        let d = weak_copy_error(self.cx, t, &what, span);
        self.cx.error(d);
    }
}

/// The error for `what` copying a value of type `t`, which holds a weak collection or a `weak`
/// field, at `span` ([`FnCx::no_weak_copy`]; also `crate::weak_threads` and
/// `ownership::locked`): naming the `weak` field when that is all `t` holds.
pub(crate) fn weak_copy_error(
    cx: &mut crate::ctx::Ctx,
    t: TyId,
    what: &GenCopy,
    span: Span,
) -> Diagnostic {
    let fields = cx.holds_weak_field_only(t);
    weak_copy_error_as(cx, t, what, span, fields)
}

/// What a value holds that belongs to its task: weak collections, or `weak` fields only.
pub(crate) struct Held {
    fields: bool,
}

impl Held {
    pub(crate) fn new(fields: bool) -> Self {
        Held { fields }
    }

    /// Who the held thing belongs to, opening a note.
    fn owner(&self) -> &'static str {
        match self.fields {
            true => {
                "a `weak` field refers to its target through the weak core of the task that set it"
            }
            false => "a `WeakMap`, `WeakSet` or `WeakRef` belongs to the task that made it",
        }
    }

    /// The captured variable `name` that holds it.
    pub(crate) fn named(&self, name: &str) -> String {
        match self.fields {
            true => format!("`{name}`, which holds a `weak` field"),
            false => format!("the weak collection `{name}`"),
        }
    }

    /// What it is, as the subject of a suggestion.
    fn gives(&self) -> &'static str {
        match self.fields {
            true => "the value with the `weak` field",
            false => "the weak collection",
        }
    }

    /// [`named`](Self::named) in the middle of a sentence (`… {} cannot be passed …`).
    fn named_mid(&self, name: &str) -> String {
        match self.fields {
            true => format!("`{name}`, which holds a `weak` field,"),
            false => format!("the weak collection `{name}`"),
        }
    }

    /// How to keep it on its task.
    fn inside(&self, place: &str) -> String {
        match self.fields {
            true => format!("make the value with the `weak` field inside {place}"),
            false => format!("make it inside {place}"),
        }
    }

    /// Why it cannot cross a `with` lock.
    pub(crate) fn lock_note(&self) -> String {
        format!(
            "the lock protects the locked value only while `with` runs, and {}: one used across the lock would let another thread release or read a part of the value without the lock; use {} outside `with`, or make it inside and drop it there",
            self.owner(),
            self.gives()
        )
    }
}

/// [`weak_copy_error`], `fields` saying whether `t` holds `weak` fields only (also through an
/// interface or base class it hides them behind: `crate::weak_threads`).
pub(crate) fn weak_copy_error_as(
    cx: &mut crate::ctx::Ctx,
    t: TyId,
    what: &GenCopy,
    span: Span,
    fields: bool,
) -> Diagnostic {
    let tn = cx.display(t);
    let held = Held::new(fields);
    let owner = held.owner();
    let direct = matches!(cx.ty.kind(t), TyKind::Adt(d, _) if cx.is_weak_class(*d));
    let it = match (direct, fields) {
        (true, _) => format!("a weak collection (`{tn}`)"),
        (false, true) => format!("`{tn}`, which holds a `weak` field,"),
        (false, false) => format!("`{tn}`, which holds a weak collection,"),
    };
    // A class with a `weak` field (velt_sema `weak_types`): copying one maps the field to its
    // target's copy or to `null` once #3119 gets there.
    let weak_field = matches!(cx.ty.kind(t), TyKind::Adt(d, _)
        if matches!(&cx.info[d.0 as usize], crate::defs::DefInfo::Adt(a) if a.fields.iter().any(|f| f.weak)));
    if weak_field
        && matches!(
            what,
            GenCopy::Clone | GenCopy::Task | GenCopy::TaskPromise | GenCopy::Shared
        )
    {
        let (verb, why) = match what {
            GenCopy::Clone => ("copied", "cloning"),
            GenCopy::Task | GenCopy::TaskPromise => {
                ("passed to another task", "passing to another task")
            }
            _ => ("shared between threads", "sharing between threads"),
        };
        return Diagnostic::error(format!("`{tn}`, which has a `weak` field, cannot be {verb}"), span)
            .with_note(format!(
                "a `weak` field refers to its target through the weak core of the task that has the target; {why} a value with one is not implemented yet (#3119)"
            ));
    }
    let (msg, note) = match what {
        GenCopy::Clone => (
            format!("{it} cannot be copied"),
            match fields {
                true => format!("{owner}, and a copy would share its slot: copy the values it refers to instead"),
                false => "a `WeakMap`, `WeakSet` or `WeakRef` cannot be cloned (in JavaScript, `structuredClone` rejects it too): pass it on, or make a new one".to_string(),
            },
        ),
        GenCopy::Task | GenCopy::TaskPromise => (
            format!("{it} cannot be passed to another task"),
            format!("{owner}, and values passed to a spawned task (or sent on a channel) are copied for it: {}", held.inside("the task")),
        ),
        GenCopy::TaskFn(name) => (
            format!("a function that uses {} cannot be passed to another task", held.named_mid(name)),
            format!("{owner}, and a function passed to a spawned task (or sent on a channel) takes what it captures to that task's thread: {}", held.inside("the task")),
        ),
        GenCopy::Shared => (
            format!("{it} cannot be shared between threads"),
            format!("{owner}, and a `shared(...)` value may be used by several tasks at once"),
        ),
        GenCopy::Capture(name) => (
            format!("an async closure cannot capture {}", held.named(name)),
            format!("an async closure that may run as a task on another thread copies what it captures, and {owner}: {}, or pass it to an async function", held.inside("the closure")),
        ),
        GenCopy::LockCapture(name) => (
            format!("the function passed to `with` cannot use {}", held.named(name)),
            held.lock_note(),
        ),
        GenCopy::LockFn(name) => (
            format!("a function stored into the locked value cannot capture {}", held.named(name)),
            match fields {
                true => format!("the locked value is used by other threads (under the lock), and {owner}: a function there that uses one would read it from another thread; store the values of its target instead"),
                false => "the locked value is used by other threads (under the lock), and a `WeakMap`, `WeakSet` or `WeakRef` belongs to the task that made it: a function there that uses one would release or read it from another thread; store the weak collection's results instead".to_string(),
            },
        ),
        GenCopy::LockResult => (format!("{it} cannot be returned from `with`"), held.lock_note()),
        GenCopy::TaskResult => (
            format!("{it} cannot be the result of a spawned task"),
            match fields {
                true => format!("{owner}, and a spawned task's result is copied for the task that awaits it: return the values of its target instead"),
                false => "a `WeakMap`, `WeakSet` or `WeakRef` belongs to the task that made it, and a spawned task's result is copied for the task that awaits it: return what the weak collection gives (`ref.deref()`, `map.get(k)`) instead".to_string(),
            },
        ),
        GenCopy::TaskError => (
            format!("{it} cannot be thrown from a spawned task"),
            match fields {
                true => format!("{owner}, and the error a spawned task throws is copied for the task that awaits it: put the values of its target in the error instead"),
                false => "a `WeakMap`, `WeakSet` or `WeakRef` belongs to the task that made it, and the error a spawned task throws is copied for the task that awaits it: put what the weak collection gives in the error instead".to_string(),
            },
        ),
    };
    Diagnostic::error(msg, span).with_note(note)
}
