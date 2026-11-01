//! Notes for an interface type expected where a value of another type is given
//! (`coerce.rs` `report_mismatch`).

use crate::body::FnCx;
use crate::hir::{TyId, TyKind};

impl FnCx<'_, '_> {
    /// For an interface value where an interface it extends is expected (`IterableIterator<T>`
    /// for `Iterator<T>`): such values don't convert yet.
    pub(super) fn extends_note(&mut self, expected: TyId, found: TyId) -> Option<String> {
        let (TyKind::Dyn(want, _), TyKind::Dyn(have, _)) = (
            self.cx.ty.kind(expected).clone(),
            self.cx.ty.kind(found).clone(),
        ) else {
            return None;
        };
        let parents = self.cx.iface(have)?.parents.clone();
        if !parents.iter().any(|p| p.iface == want) {
            return None;
        }
        let (e, f) = (self.cx.display(expected), self.cx.display(found));
        let fix = match Some(want) == self.cx.prelude_iface("Iterator") {
            true => "; `x[Symbol.iterator]()` gives its `Iterator`".to_string(),
            false => format!(": pass the value it was made from, or take a `{f}`"),
        };
        Some(format!(
            "`{f}` extends `{e}`, but an interface value does not convert to the interfaces it extends yet{fix}"
        ))
    }

    /// For a value implementing the expected interface with other type arguments (`string[]`
    /// where an `Iterable<f64>` is expected): which ones.
    pub(super) fn other_args_note(&mut self, expected: TyId, found: TyId) -> Option<String> {
        let TyKind::Dyn(want, _) = self.cx.ty.kind(expected).clone() else {
            return None;
        };
        let (_, args, _) = self.cx.find_impl(found, want)?;
        let has = self.cx.ty.intern(TyKind::Dyn(want, args));
        let (e, f, h) = (
            self.cx.display(expected),
            self.cx.display(found),
            self.cx.display(has),
        );
        // The same interface with the same written arguments (`any` against `any`): no note.
        (h != e).then(|| format!("`{f}` is an `{h}`, not an `{e}`"))
    }
}
