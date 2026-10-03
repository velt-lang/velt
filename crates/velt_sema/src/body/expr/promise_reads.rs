//! Reading a promise out of a collection: an array element moved out by indexing, or a method
//! of `Map` / arrays that returns copies of the elements (`m.get(k)`, `arr.at(i)`,
//! `arr.slice()`, …). A promise has one owner and can't be copied, so these are compile-time
//! errors pointing at what works instead.

use velt_common::{Diagnostic, Span};

use crate::body::FnCx;
use crate::hir::{TyId, TyKind};

/// `Map` methods whose result copies stored values.
const MAP_COPYING: &[&str] = &["get", "getOrInsert", "values", "entries"];

/// Array methods whose result (or the array they fill) copies elements.
const ARRAY_COPYING: &[&str] = &[
    "at",
    "concat",
    "entries",
    "fill",
    "filter",
    "find",
    "findLast",
    "slice",
    "toReversed",
    "toSorted",
    "toSpliced",
    "with",
];

impl FnCx<'_, '_> {
    /// `arr[i]` read where the element holds a promise.
    pub(super) fn promise_out_of_array(&mut self, elem: TyId, span: Span) {
        let tn = self.cx.display(elem);
        self.cx.error(
            Diagnostic::error(
                format!("cannot read a `{tn}` out of an array element"),
                span,
            )
            .with_note(
                "a promise has one owner and can't be copied: take it out of the array with \
                 `pop()` or `splice(i, 1)`, or await the promises together with \
                 `Promise.all(arr)`",
            ),
        );
    }

    /// `recv.method(…)` on a `Map` or an array whose values hold a promise, for a method that
    /// copies them: reported (returns whether it was).
    pub(super) fn reject_promise_copying_method(
        &mut self,
        recv_ty: TyId,
        method: &str,
        span: Span,
    ) -> bool {
        let map = self.cx.prelude_adt("Map");
        let (held, what, note) = match self.cx.ty.kind(recv_ty).clone() {
            TyKind::Array(elem) if ARRAY_COPYING.contains(&method) => (
                elem,
                "array",
                "a promise has one owner and can't be copied: take promises out with `pop()` or \
                 `splice(i, 1)`, or await them together with `Promise.all(arr)`",
            ),
            TyKind::Adt(d, args)
                if Some(d) == map && args.len() == 2 && MAP_COPYING.contains(&method) =>
            {
                (
                    args[1],
                    "`Map`",
                    "a promise has one owner and can't be copied: store the awaited result \
                     (`m.set(k, await p)`), or keep the promises in an array and take them out \
                     with `pop()`",
                )
            }
            _ => return false,
        };
        if !self.cx.holds_promise(held) {
            return false;
        }
        let tn = self.cx.display(held);
        self.cx.error(
            Diagnostic::error(
                format!("`{method}` would copy a `{tn}` out of the {what}"),
                span,
            )
            .with_note(note),
        );
        true
    }
}
