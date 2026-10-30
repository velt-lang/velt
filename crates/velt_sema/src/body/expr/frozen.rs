//! Reading a `shared<T>` like the `T` it holds (#36). A `shared<T>` without a `Mutex` is
//! immutable: every task may read it at once. So `s[i]`, `s.length`, a method of `T` called on
//! `s` and `s` passed where a `T` is expected read the value inside the shared box
//! (`Intrinsic::SharedRead`) instead of being errors. Lowering borrows the value when nothing in
//! `T` is reference counted (those counts are not atomic, so other threads' readers must not
//! touch them) and gives the reader its own copy otherwise; a reader that keeps the value gets a
//! copy too. Writing through the view is an error (`frozen_write`), and so is a call that may
//! modify it (`ownership/patch.rs`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::places::{frozen_root, set_place_mode};
use crate::body::{FnCx, Want};
use crate::hir::{self, Callee, ExprKind as H, Intrinsic, TyId, TyKind, UseMode};

impl FnCx<'_, '_> {
    /// The object of `s[i]`: a `shared<string>`, `shared<T[]>` or `shared` typed array read as
    /// the value inside (writes through an array's element are reported where it is assigned);
    /// `Err` with the error expression for a write through a typed array's indexer, which those
    /// checks don't see.
    pub(super) fn shared_indexed(
        &mut self,
        obj: hir::Expr,
        index: &ast::Expr,
        want: Want,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let TyKind::Shared(t) = *self.cx.ty.kind(obj.ty) else {
            return Ok(obj);
        };
        if matches!(want, Want::BorrowMut) && self.has_indexer(t) {
            self.frozen_write(&obj, span);
            self.expr(index, None, Want::Borrow);
            return Err(self.error_expr(span));
        }
        if self.has_length(t) || self.has_indexer(t) {
            return Ok(self.frozen_view(obj, None));
        }
        Ok(obj)
    }

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
        if !self.frozen_readable_here(inner) {
            return h;
        }
        // `velt/sync/epoch`, which the prelude imports for every program, reads its registry
        // in place: that alone does not make a program one that reads `shared` values.
        let m = &self.cx.modules[self.module];
        if !(m.is_std && m.path == "std/sync/epoch/reclaim") {
            self.cx.frozen_reads = true;
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

    /// `s.f` on a `shared<T>` of an object type with a field or getter `f`, or of a type
    /// parameter (its bounds' members, `bound_member`): the value inside's (#36). A getter is a
    /// call on it, refused where it may modify it, as a method is (`ownership/patch.rs`).
    pub(crate) fn shared_member_view(&mut self, obj: hir::Expr, name: &str) -> hir::Expr {
        let TyKind::Shared(t) = *self.cx.ty.kind(obj.ty) else {
            return obj;
        };
        if matches!(self.cx.ty.kind(t), TyKind::Param(_))
            || self.field_of(t, name).is_some()
            || self.has_getter(t, name)
        {
            return self.frozen_view(obj, Some(name));
        }
        obj
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

    /// Is `ty` a `shared<T>` that is not read like its `T` (other than a `shared<int>`, whose
    /// methods are its atomics)?
    pub(crate) fn unreadable_shared(&mut self, ty: hir::TyId) -> bool {
        match *self.cx.ty.kind(ty) {
            TyKind::Shared(t) => !self.cx.ty.is_int(t) && !self.frozen_readable_here(t),
            _ => false,
        }
    }

    /// Why the `shared<T>` type `shared` (one `frozen_readable` refuses) is not read like its `T`.
    pub(crate) fn unreadable_shared_note(&mut self, shared: hir::TyId) -> String {
        let f = self.cx.display(shared);
        let inner = match *self.cx.ty.kind(shared) {
            TyKind::Shared(t) => Some(t),
            _ => None,
        };
        let e = inner.map_or_else(|| f.clone(), |t| self.cx.display(t));
        if inner.is_some_and(|t| matches!(self.cx.ty.kind(t), TyKind::Param(_))) {
            return format!(
                "`{f}` is read like a `{e}` only when `{e}` is bounded by `Frozen` (`{e} extends Frozen`, checked where the generic is used)"
            );
        }
        format!(
            "`{f}` cannot be read like a `{e}`: other threads read it at the same time, so it \
             is read in place only when nothing in `{e}` could change it or run code on it \
             (it holds a `Mutex`, a weak reference, a promise, a function or interface value, a \
             type parameter, or a class with subclasses or with a `clone()` of its own)"
        )
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

    /// `subarray()` or `buffer` of a typed array or `DataView` read out of a `shared` (an
    /// error, reported): JavaScript's result shares the shared value's buffer, so a write
    /// through it would change that value, where a reader's copy would take it silently.
    pub(crate) fn frozen_buffer_view(&mut self, obj: &hir::Expr, member: &str, span: Span) -> bool {
        if !matches!(member, "subarray" | "buffer") || !frozen_root(obj) {
            return false;
        }
        let view = self.cx.class_of(obj.ty).is_some_and(|(d, _)| {
            self.cx
                .adt(d)
                .is_some_and(|a| a.qual_name.starts_with("std/lang/typed_array/"))
        });
        if !view {
            return false;
        }
        let tn = self.cx.display(obj.ty);
        self.cx.error(
            Diagnostic::error(
                format!(
                    "`{member}` of a `{tn}` read out of a `shared` value would share its buffer"
                ),
                span,
            )
            .with_note(
                "a shared value is read-only, since other tasks read it at the same time: use \
                 `slice()` for a copy, or share a `Mutex`",
            ),
        );
        true
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

impl FnCx<'_, '_> {
    /// `Ctx::frozen_readable` with this body's type parameters bounded by the prelude's `Frozen`
    /// taken as readable (checked where the generic is instantiated).
    /// The name of `t` when it is the prelude's `AtomicValue` or `Frozen` as a value type:
    /// interfaces the compiler implements, only ever bounds.
    pub(crate) fn bound_only_iface(&self, t: TyId) -> Option<&'static str> {
        let TyKind::Dyn(d, _) = self.cx.ty.kind(t) else {
            return None;
        };
        let d = *d;
        [
            ("AtomicValue", self.cx.prelude_iface("AtomicValue")),
            ("Frozen", self.cx.prelude_iface("Frozen")),
        ]
        .into_iter()
        .find(|(_, i)| *i == Some(d))
        .map(|(n, _)| n)
    }

    pub(crate) fn frozen_readable_here(&mut self, t: TyId) -> bool {
        self.cx.frozen_readable_under(&self.bounds, t)
    }
}
