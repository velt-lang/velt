//! What velt/ffi's `Ptr.read<S>` and `Ptr.write<S>` move (`ptrRead`, `ptrWrite`, #3094): the
//! bytes of one `S` in C memory, so `S` is a type whose Velt layout is C's. Checked at the
//! intrinsic's call for a concrete `S` and where a type parameter is instantiated
//! (`json`'s `PtrUse::Memory`).

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::hir::{IntTy, Nothing, TyId, TyKind};

impl Ctx<'_> {
    /// Reports an `s` that `Ptr.read` and `Ptr.write` can't move: one that isn't a number type,
    /// `bool`, a C pointer or a C struct, or (on wasm32) one whose layout isn't C's there. True if
    /// it reported. A type parameter passes: its instances are checked where it is instantiated.
    pub(crate) fn c_memory_refused(&mut self, s: TyId, span: Span) -> bool {
        if self.ty.has_error(s) || matches!(self.ty.kind(s), TyKind::Param(_)) {
            return false;
        }
        let shown = self.display(s);
        if !self.c_memory_ty(s) {
            let note = match self.ty.kind(s) {
                TyKind::Unit => "a `Ptr<void>` points at bytes of no type: read them through a `Ptr<u8>`, or a pointer to the type that is there",
                _ if self.is_c_type_ty(s) => "a `declare c type` is opaque: only C knows its size and fields, so only C reads or writes one",
                _ => "it reads or writes the bytes of a number type, `bool`, a C pointer or a `declare c struct`, as C lays them out",
            };
            self.error(
                Diagnostic::error(
                    format!("`Ptr.read` and `Ptr.write` can't move a `{shown}`"),
                    span,
                )
                .with_note(note),
            );
            return true;
        }
        if self.wasm32 && self.word_sized_in(s) {
            self.error(
                Diagnostic::error(
                    format!("`Ptr.read` and `Ptr.write` of a `{shown}` aren't available on wasm32"),
                    span,
                )
                .with_note("C's pointers, `size_t` and `ssize_t` are 4 bytes on wasm32, and Velt's `Ptr<T>`, `usize` and `isize` 8, so the layout isn't C's there; a C struct of other fields is"),
            );
            return true;
        }
        false
    }

    /// Is `t` a number type, `bool`, a C pointer (`T | null | undefined` isn't one word) or a C
    /// struct?
    fn c_memory_ty(&self, t: TyId) -> bool {
        match self.ty.kind(t) {
            TyKind::Bool | TyKind::Ptr(_) => true,
            TyKind::Option(inner, n) => {
                *n != Nothing::Both && matches!(self.ty.kind(*inner), TyKind::Ptr(_))
            }
            _ => self.ty.is_numeric(t) || self.ty.is_storage_float(t) || self.is_c_struct_ty(t),
        }
    }

    /// Does `t` hold a C pointer, a `usize` or an `isize`, in nested C structs too: what is 8
    /// bytes in Velt and 4 in C on wasm32?
    fn word_sized_in(&self, t: TyId) -> bool {
        match self.ty.kind(t) {
            TyKind::Int(IntTy::USize | IntTy::ISize) | TyKind::Ptr(_) => true,
            TyKind::Option(inner, _) => self.word_sized_in(*inner),
            TyKind::Adt(d, _) if self.is_c_struct(*d) => self
                .adt(*d)
                .is_some_and(|a| a.fields.iter().any(|f| self.word_sized_in(f.declared))),
            _ => false,
        }
    }

    /// Is `t` a `declare c type`?
    fn is_c_type_ty(&self, t: TyId) -> bool {
        matches!(self.ty.kind(t), TyKind::Adt(d, _) if self.adt(*d).is_some_and(|a| a.decl.is_some_and(|t| t.c_type)))
    }
}
