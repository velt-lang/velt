//! `switch` checks: duplicate cases, and exhaustiveness. A `switch` without `default` on a
//! discriminant, a `typeof` of a union, a union of literals or an enum must have a case for
//! every member the value can be (flow narrowing included); missing cases are an error listing
//! them. A `default` body sees the value narrowed to the remaining members, so
//! `const _x: never = s;` there checks exhaustiveness TS-style.

use velt_common::{Diagnostic, Span};

use super::cases::{Scrut, ScrutKind};
use super::select::Sel;
use crate::body::FnCx;

impl FnCx<'_, '_> {
    /// Report cases that select only values an earlier case already selects.
    pub(super) fn check_duplicates(&mut self, sels: &[(Option<Sel>, Span)]) {
        let mut covered: Vec<usize> = vec![];
        let mut lits = vec![];
        for (sel, span) in sels {
            let Some(sel) = sel else {
                continue;
            };
            if sel.guard.is_none()
                && !sel.covered.is_empty()
                && sel.covered.iter().all(|k| covered.contains(k))
            {
                self.cx.err(
                    "duplicate `case`: an earlier case already handles this value",
                    *span,
                );
            }
            if let Some(l) = &sel.lit {
                if lits.contains(l) {
                    self.cx.err("duplicate `case` value", *span);
                }
                lits.push(l.clone());
            }
            covered.extend(sel.covered.iter().copied());
        }
    }

    /// Must every member have a case (when there is no `default`)?
    pub(super) fn needs_exhaustive(&mut self, s: &Scrut) -> bool {
        match &s.kind {
            ScrutKind::Discriminant(_) | ScrutKind::Enum(_) => true,
            ScrutKind::TypeOf => s.slots.len() > 1,
            ScrutKind::Union => s
                .slots
                .iter()
                .all(|slot| slot.member.is_none_or(|m| self.cx.lit_value(m).is_some())),
            ScrutKind::Plain => false,
        }
    }

    /// Slots no case covers.
    pub(super) fn uncovered(s: &Scrut, sels: &[(Option<Sel>, Span)]) -> Vec<usize> {
        (0..s.slots.len())
            .filter(|k| {
                !sels
                    .iter()
                    .any(|(sel, _)| sel.as_ref().is_some_and(|x| x.covered.contains(k)))
            })
            .collect()
    }

    /// "non-exhaustive switch" with the missing cases.
    pub(super) fn report_missing(&mut self, s: &Scrut, missing: &[usize], span: Span) {
        let mut names: Vec<String> = vec![];
        for &k in missing {
            let n = s.slots[k].name.clone();
            if !names.contains(&n) {
                names.push(n);
            }
        }
        let cases: Vec<String> = names.iter().map(|n| format!("`case {n}:`")).collect();
        self.cx.error(
            Diagnostic::error(format!("non-exhaustive switch on `{}`", s.what), span)
                .with_note(format!("missing cases: {}", names.join(", ")))
                .with_note(format!("add {} or a `default:` clause", cases.join(", "))),
        );
    }
}
