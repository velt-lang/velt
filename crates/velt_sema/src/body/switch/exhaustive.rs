//! Coverage of a `switch`: the members (slots) no case selects. As in TypeScript, a `switch`
//! need not cover every member, and a duplicate case is allowed (the first one wins). A switch
//! that covers every member needs no `default` and no code after it; without `default`, the
//! members left over skip it, so a `default` body (or the code after the `switch`) sees the
//! value narrowed to them, and `const _x: never = s;` there checks exhaustiveness the
//! TypeScript way.

use velt_common::Span;

use super::cases::Scrut;
use super::select::Sel;
use crate::body::FnCx;

impl FnCx<'_, '_> {
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
}
