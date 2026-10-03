//! Reading a promise out of an array where sema sees the element type itself: moving an element
//! out by indexing (`await arr[i]`), binding one by reference (`const p = arr[i]`), spreading
//! (`[...arr]`) and destructuring (`const [a, b] = arr`). TypeScript allows these; a Velt
//! promise has one owner and can't be copied or shared, so they are compile-time errors that say
//! so and point at what works instead. Copies made inside generic code (`m.get(k)`,
//! `arr.at(i)`, …) are found per instantiation by `crate::promise_copies`.

use velt_common::{Diagnostic, Span};

use crate::body::FnCx;
use crate::hir::{TyId, TyKind};
use crate::promise_copies::WHY;

impl FnCx<'_, '_> {
    /// The note for reading `elem` out of an array: a promise, or a value holding one.
    fn array_read_note(&mut self, elem: TyId) -> String {
        if matches!(self.cx.ty.kind(elem), TyKind::Promise(..)) {
            format!(
                "{WHY}: take it out of the array with `pop()` or `splice(i, 1)`, or \
                 await the promises together with `Promise.all(arr)`"
            )
        } else {
            format!(
                "{WHY}, nor can a value that holds one: read its fields in place \
                 (`arr[i].field`), or take it out with `pop()` or `splice(i, 1)`"
            )
        }
    }

    /// Is `t` a promise (possibly `| null`) itself, which a by-reference binding could only be
    /// awaited through, not an object that holds one (whose other fields read in place)?
    pub(in crate::body) fn binds_promise(&mut self, t: TyId) -> bool {
        let t = self.cx.ty.opt_payload(t).unwrap_or(t);
        matches!(self.cx.ty.kind(t), TyKind::Promise(..))
    }

    /// `arr[i]` moved out, or bound by reference, where the element holds a promise.
    pub(in crate::body) fn promise_out_of_array(&mut self, elem: TyId, span: Span) {
        let tn = self.cx.display(elem);
        let note = self.array_read_note(elem);
        self.cx.error(
            Diagnostic::error(
                format!("cannot read a `{tn}` out of an array element"),
                span,
            )
            .with_note(note),
        );
    }

    /// `[...arr]` / `f(...arr)` copying elements that hold a promise: reported (returns whether
    /// it was).
    pub(in crate::body) fn reject_promise_spread(&mut self, elem: TyId, span: Span) -> bool {
        if !crate::promise_copies::share_copies_promise(self.cx, elem) {
            return false;
        }
        let tn = self.cx.display(elem);
        let note = self.array_read_note(elem);
        self.cx.error(
            Diagnostic::error(format!("spreading the array would copy a `{tn}`"), span)
                .with_note(note),
        );
        true
    }

    /// `const [a, b] = arr` binding elements that hold a promise: reported.
    pub(in crate::body) fn reject_promise_destructuring(&mut self, elem: TyId, span: Span) {
        if !crate::promise_copies::share_copies_promise(self.cx, elem) {
            return;
        }
        let tn = self.cx.display(elem);
        let note = self.array_read_note(elem);
        self.cx.error(
            Diagnostic::error(format!("destructuring the array would copy a `{tn}`"), span)
                .with_note(note),
        );
    }
}
