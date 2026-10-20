//! Reading a `shared<T>` like the `T` it holds (#36). A `shared<T>` without a `Mutex` is
//! immutable: every task may read it at once. So `s[i]`, `s.length`, a method of `T` called on
//! `s` and `s` passed where a `T` is expected read the value inside the shared box
//! (`Intrinsic::SharedRead`) instead of being errors. Lowering borrows the value when nothing in
//! `T` is reference counted (those counts are not atomic, so other threads' readers must not
//! touch them) and gives the reader its own copy otherwise; a reader that keeps the value gets a
//! copy too. Writing through the view is an error (`frozen_write`), and so is a call that may
//! modify it (`ownership/patch.rs`).

use velt_common::Span;

use crate::body::places::set_place_mode;
use crate::body::FnCx;
use crate::hir::{self, Callee, ExprKind as H, Intrinsic, TyKind, UseMode};

impl FnCx<'_, '_> {
    /// `h` as the `T` of its `shared<T>`, for a use that only reads it (`member`: the member
    /// read or called on it, which a `shared` value may have itself); anything else as it is.
    pub(crate) fn frozen_view(&mut self, h: hir::Expr, member: Option<&str>) -> hir::Expr {
        let TyKind::Shared(inner) = *self.cx.ty.kind(h.ty) else {
            return h;
        };
        // `s.clone()` is another reference, and a `shared<int>`'s `add`/`get`/`set` are atomic:
        // reading the int as a plain value would race with them.
        if member == Some("clone") || self.cx.atomic_int(h.ty).is_some() {
            return h;
        }
        if !self.cx.frozen_readable(inner) {
            return h;
        }
        let span = h.span;
        let mut h = h;
        set_place_mode(&mut h, UseMode::Borrow);
        let call = H::Call {
            callee: Callee::Intrinsic(Intrinsic::SharedRead),
            args: vec![h],
        };
        self.mk(call, inner, span)
    }

    /// `h`, or the value inside when it is a `shared<T>` that can be read like its `T`: for
    /// what takes any value and reads it (printing, spreading, `Object.keys`), as JavaScript
    /// sees the `T` itself.
    pub(crate) fn read_shared(&mut self, h: hir::Expr) -> hir::Expr {
        match self.cx.ty.kind(h.ty) {
            TyKind::Shared(_) => self.frozen_view(h, None),
            _ => h,
        }
    }

    /// Is `h` a value read out of a `shared<T>` (`frozen_view`)?
    pub(crate) fn is_frozen_view(h: &hir::Expr) -> bool {
        matches!(
            h.kind,
            H::Call {
                callee: Callee::Intrinsic(Intrinsic::SharedRead),
                ..
            }
        )
    }

    /// Report a write at `span` through a value read out of `shared`.
    pub(crate) fn frozen_write(&mut self, shared: &hir::Expr, span: Span) {
        let tn = self.cx.display(shared.ty);
        self.cx.err(
            format!("cannot modify a `{tn}`: a shared value is read-only, since other tasks read it at the same time"),
            span,
        );
    }
}
