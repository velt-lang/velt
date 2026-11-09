//! The intrinsics on raw memory (std/lang/intrinsics/address.velt and atomic.velt's
//! `atomicAddress`, docs/internals/design/intrinsics.md "Memory at an address"): what their
//! declarations cannot say. `addressLoad<T>` and `addressStore<T>` move one number or `bool`;
//! the `ptrAtomic*` ones work on an integer, and their orderings (and `ptrAtomicRmw`'s operation)
//! are string literals that become the integer literals the `Atomic*` intrinsics take; and
//! `atomicAddress` takes only an `Atomic` that cannot move.

use velt_common::{Diagnostic, Span};

use super::atomic::RMW;
use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, Intrinsic, TyId, TyKind};

impl FnCx<'_, '_> {
    /// The checked arguments of the raw-memory intrinsic `i` (whose type arguments are
    /// `type_args`) as lowering takes them; `None` after reporting an error.
    pub(super) fn raw_memory_args(
        &mut self,
        i: Intrinsic,
        type_args: &[TyId],
        args: Vec<hir::Expr>,
        span: Span,
    ) -> Option<Vec<hir::Expr>> {
        use Intrinsic as I;
        let t = type_args.first().copied();
        match i {
            I::AddressLoad | I::AddressStore => {
                let ok = t.is_none_or(|t| self.raw_value_ty(t, false));
                self.raw_type_error(i, t, ok, "a number type or `bool`", span)?;
                Some(args)
            }
            I::PtrAtomicLoad
            | I::PtrAtomicStore
            | I::PtrAtomicRmw
            | I::PtrAtomicCas
            | I::PtrAtomicCasWeak => {
                let ok = t.is_none_or(|t| self.raw_value_ty(t, true));
                self.raw_type_error(i, t, ok, "an integer type", span)?;
                self.ptr_atomic_args(i, args)
            }
            I::AtomicAddress => {
                let a = args.first()?;
                self.stable_atomic(a, span)?;
                Some(args)
            }
            _ => Some(args),
        }
    }

    /// Can a raw read or write move a `T`: a number type or `bool` (an integer type, for an
    /// atomic one)? A type parameter of a generic std function passes: sema checks the body once,
    /// and lowering checks each instance (another type is an ICE there).
    fn raw_value_ty(&self, t: TyId, int_only: bool) -> bool {
        let ty = &self.cx.ty;
        if ty.is_bottom(t) || matches!(ty.kind(t), TyKind::Param(_) | TyKind::Error) {
            return true;
        }
        match int_only {
            true => matches!(ty.kind(t), TyKind::Int(_)),
            false => {
                ty.is_numeric(t) || ty.is_storage_float(t) || matches!(ty.kind(t), TyKind::Bool)
            }
        }
    }

    /// Reports the `T` of intrinsic `i` unless `ok`.
    fn raw_type_error(
        &mut self,
        i: Intrinsic,
        t: Option<TyId>,
        ok: bool,
        needs: &str,
        span: Span,
    ) -> Option<()> {
        if ok {
            return Some(());
        }
        let name = crate::intrinsic_decls::name_of(i).unwrap_or("intrinsic");
        let shown = t.map(|t| self.cx.display(t)).unwrap_or_default();
        self.cx.error(
            Diagnostic::error(format!("`{name}<T>` needs {needs} for `T`, found `{shown}`"), span)
                .with_note("it reads or writes the bytes of one value at an address: nothing that owns memory or holds a reference"),
        );
        None
    }

    /// The arguments of a `ptrAtomic*` call with its string literal orderings (and operation)
    /// as the `Atomic*` intrinsics' integer literals: `(p, order)`, `(p, v, order)`,
    /// `(p, v, op, order)` for `ptrAtomicRmw(p, op, v, order)`, `(p, expected, v, order,
    /// failure)`.
    fn ptr_atomic_args(
        &mut self,
        i: Intrinsic,
        mut args: Vec<hir::Expr>,
    ) -> Option<Vec<hir::Expr>> {
        let literals = match i {
            Intrinsic::PtrAtomicLoad => vec![1],
            Intrinsic::PtrAtomicStore => vec![2],
            Intrinsic::PtrAtomicRmw => vec![1, 3],
            _ => vec![3, 4],
        };
        for at in literals {
            let a = args.get(at)?;
            if matches!(self.cx.ty.kind(a.ty), TyKind::Error) {
                return None;
            }
            let n = match (i, at) {
                (Intrinsic::PtrAtomicRmw, 1) => self
                    .literal_string(a)
                    .and_then(|s| RMW.iter().position(|m| *m == s)),
                _ => self.ordering_of(a).map(usize::from),
            };
            let Some(n) = n else {
                let what = match (i, at) {
                    (Intrinsic::PtrAtomicRmw, 1) => "an operation",
                    _ => "an ordering",
                };
                self.cx.error(
                    Diagnostic::error(format!("{what} must be a string literal"), a.span)
                        .with_note("the compiler picks the instruction for it"),
                );
                return None;
            };
            args[at] = self.u8_lit(n as u8, a.span);
        }
        if i == Intrinsic::PtrAtomicRmw {
            // `(p, op, v, order)` → `AtomicRmw`'s `(p, v, op, order)`.
            args.swap(1, 2);
        }
        Some(args)
    }

    /// Checks that `a`, the argument of `atomicAddress`, is an `Atomic` of a 32- or 64-bit
    /// integer that cannot move: a static atomic (a module `const` made by
    /// `new Atomic<T>(literal)`, whose word is in the program's data), or a field (of a field…)
    /// of the value in a `shared` box, read in place (the box is not freed while a reference to
    /// it lives, and nothing in it moves).
    fn stable_atomic(&mut self, a: &hir::Expr, span: Span) -> Option<()> {
        let t = self.cx.atomic_value(a.ty);
        let wide =
            t.is_some_and(|t| matches!(self.cx.ty.kind(t), TyKind::Int(n) if n.bits() >= 32));
        if t.is_some_and(|t| matches!(self.cx.ty.kind(t), TyKind::Error)) {
            return None;
        }
        if !wide {
            let shown = self.cx.display(a.ty);
            self.cx.error(
                Diagnostic::error(
                    format!("`atomicAddress` takes an `Atomic` of a 32- or 64-bit integer, found `{shown}`"),
                    span,
                )
                .with_note("the address is for the OS's wait and wake calls on one word"),
            );
            return None;
        }
        if self.static_atomic_global(a) || in_shared_box(a) {
            return Some(());
        }
        self.cx.error(
            Diagnostic::error("`atomicAddress` needs an `Atomic` that cannot move", a.span).with_note(
                "a module `const` made by `new Atomic<T>(literal)`, or a field of the value of a `shared` box (`s.state`); a local, an object or an array moves or is freed while its address is in use",
            ),
        );
        None
    }

    /// Is `a` a static atomic: a module `const` (not a static field) whose initializer is
    /// `new Atomic<T>(literal)` and which is not kept in the module state (sema's
    /// `static_atomic_init`, velt_vir `lower/static_atomic.rs`)?
    fn static_atomic_global(&mut self, a: &hir::Expr) -> bool {
        let H::Global(d) = a.kind else {
            return false;
        };
        crate::body::driver::ensure_global(self.cx, d);
        let Some(g) = self.cx.global(d) else {
            return false;
        };
        !g.mutable
            && !g.stored
            && g.src.owner.is_none()
            && g.init
                .as_ref()
                .is_some_and(|i| super::static_atomic_init(self.cx, i))
    }
}

/// Is `a` a field (of a field…) of the value of a `shared` box, read in place?
fn in_shared_box(a: &hir::Expr) -> bool {
    match &a.kind {
        H::Field { base, .. } => match &base.kind {
            H::Call {
                callee: hir::Callee::Intrinsic(Intrinsic::SharedRead),
                ..
            } => true,
            _ => in_shared_box(base),
        },
        _ => false,
    }
}
