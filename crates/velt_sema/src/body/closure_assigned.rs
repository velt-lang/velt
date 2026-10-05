//! Variables that closures assign are not narrowed. A call (or an `await`, a getter, …) between
//! a check and a use may run a closure that reassigns the variable, so no check proves anything
//! about its later reads: `if (x instanceof Sub) { reset(); x.tail = 1; }` would write past a
//! `Base` object, and `if (g !== null) { clear(); g.m(); }` would call a method on `null`.
//! TypeScript keeps such narrowings (JavaScript then reads `undefined` or throws); Velt drops
//! them for every variable some closure assigns (`assigned::assigned_by_closures`, by name, per
//! function), and an error at a read the check would have allowed says why and how to fix it:
//! test a `const` copy, which nothing can reassign.
//!
//! Null checks of field paths (`k.left !== null`) still narrow: their reads are checked again
//! (`field_narrow`). `instanceof` on a `readonly` field path of such a variable does not.

use std::collections::HashMap;

use velt_common::{Label, Span};

use super::narrow::Fact;
use super::FnCx;
use crate::hir::LocalId;

/// A fact not assumed because a closure assigns its variable: the note for errors at the reads
/// it would have narrowed, and where a closure assigns the variable.
#[derive(Clone)]
pub(crate) struct Refused {
    pub note: String,
    pub at: Span,
}

/// The names `assigned_by_closures` found, owned (for `Frame::closure_assigned`).
pub(crate) fn owned(names: super::assigned::Assigned) -> HashMap<String, Span> {
    names.into_iter().map(|(n, s)| (n.to_string(), s)).collect()
}

impl FnCx<'_, '_> {
    /// Where a closure assigns local `l` (of this frame, or the variable it captures), if one
    /// does.
    pub(crate) fn closure_assignment(&self, l: LocalId) -> Option<Span> {
        let mut l = l;
        for f in std::iter::once(&self.f).chain(self.outer.iter().rev()) {
            match f.captures.iter().find(|c| c.inner == l) {
                Some(c) => l = c.outer,
                None => {
                    let name = &f.locals[l.0 as usize].name;
                    return f.closure_assigned.get(name).copied();
                }
            }
        }
        None
    }

    /// Whether `fact` is about a variable a closure assigns (or an `instanceof` fact about a
    /// field path of one): then it is not assumed, and the reads in its scope are noted.
    pub(crate) fn refuse_fact(&mut self, fact: &Fact) -> bool {
        let (l, class) = match fact {
            Fact::NonNull(l) | Fact::Members(l, _) => (*l, None),
            Fact::Class(l, t) => (*l, Some(*t)),
        };
        let (root, path) = match self.token_path(l) {
            None => (l, String::new()),
            // A narrowed nullable field is checked again where it is read.
            Some(_) if class.is_none() => return false,
            Some((root, path)) => (root, format!(".{}", path.join("."))),
        };
        let Some(at) = self.closure_assignment(root) else {
            return false;
        };
        let name = self.f.locals[root.0 as usize].name.clone();
        let note = self.refusal_note(fact, &name, &format!("{name}{path}"));
        let scope = self.f.scopes.last_mut().expect("ICE: no scope");
        if !scope.refused.iter().any(|(x, _)| *x == l) {
            scope.refused.push((l, Refused { note, at }));
        }
        true
    }

    /// The note for errors at reads of `what` (`x`, `k.ro`), a variable `name` or a field path
    /// of it, that `fact` would have narrowed.
    fn refusal_note(&mut self, fact: &Fact, name: &str, what: &str) -> String {
        let target = match fact {
            Fact::Class(_, t) => Some(*t),
            Fact::NonNull(l) => self.cx.ty.opt_payload(self.local_ty(*l)),
            Fact::Members(l, vs) => match vs.as_slice() {
                [v] => {
                    let ty = self.local_ty(*l);
                    let u = self.cx.ty.opt_payload(ty).unwrap_or(ty);
                    self.cx
                        .union_members(u)
                        .and_then(|ms| ms.get(*v as usize).copied())
                }
                _ => None,
            },
        };
        let target = target.map(|t| self.cx.display(t));
        let hint = match fact {
            Fact::Class(..) => target.as_deref().unwrap_or("current"),
            _ => "current",
        };
        let copy = copy_name(hint, name);
        let copied = format!("const {copy} = {what};");
        let fix = match (fact, &target) {
            (Fact::Class(..), Some(class)) => {
                format!("`{copied} if ({copy} instanceof {class}) {{ … }}`")
            }
            (Fact::NonNull(_), _) => format!("`{copied} if ({copy} !== null) {{ … }}`"),
            _ => format!("`{copied}`, then test `{copy}`"),
        };
        let narrowed = match &target {
            Some(t) => format!("narrowed to `{t}`"),
            None => "narrowed".to_string(),
        };
        let subject = if what == name {
            "it".to_string()
        } else {
            format!("`{what}`")
        };
        format!(
            "`{name}` is assigned in a closure, so {subject} is not {narrowed} here: a call may run the closure between the check and this use; test a `const` copy instead: {fix}"
        )
    }

    /// A read of local (or field token) `l` at `span`: an error there gets the note of a fact
    /// about `l` that was not assumed.
    pub(crate) fn note_refused_read(&mut self, l: LocalId, span: Span) {
        // In a closure, a captured variable's facts were refused where the closure is created.
        let mut l = l;
        let mut found = None;
        for f in std::iter::once(&self.f).chain(self.outer.iter().rev()) {
            found = f.scopes.iter().rev().find_map(|s| {
                let r = s.refused.iter().find(|(x, _)| *x == l);
                r.map(|(_, r)| r.clone())
            });
            match f.captures.iter().find(|c| c.inner == l) {
                Some(c) if found.is_none() => l = c.outer,
                _ => break,
            }
        }
        if let Some(r) = found {
            self.refused_reads.push((span, r));
        }
    }

    /// Adds the notes of refused facts to the errors (from `diags[start..]`) at the reads they
    /// would have narrowed: an error on the read itself (`f(x)`), or on what follows it
    /// (`x.tail`, `g.m()`).
    pub(crate) fn note_refused_facts(&mut self, start: usize) {
        let reads = std::mem::take(&mut self.refused_reads);
        for d in self.cx.diags.iter_mut().skip(start) {
            let Some(p) = d.labels.first().map(|l| l.span) else {
                continue;
            };
            let hit = reads
                .iter()
                .find(|(r, _)| r.file == p.file && p.lo <= r.hi + 2 && p.hi >= r.lo);
            if let Some((_, r)) = hit {
                if !d.notes.contains(&r.note) {
                    d.labels.push(Label {
                        span: r.at,
                        message: "assigned in a closure here".to_string(),
                    });
                    d.notes.push(r.note.clone());
                }
            }
        }
    }
}

/// A name for the `const` copy of `name`: `hint` with a lowercase first letter (`Sub` → `sub`),
/// unless that is `name` itself.
fn copy_name(hint: &str, name: &str) -> String {
    let mut chars = hint.chars();
    let lower: String = match chars.next() {
        Some(c) if c.is_alphabetic() => c.to_lowercase().chain(chars).collect(),
        _ => "current".to_string(),
    };
    let ok = lower.chars().all(|c| c.is_alphanumeric() || c == '_');
    match (ok, lower == name) {
        (true, false) => lower,
        _ => format!("{name}Copy"),
    }
}
