//! `ptrRead<T>` and `ptrWrite<T>` (velt/ffi's `Ptr.read` and `Ptr.write`, #3094): a number,
//! `bool`, C struct or C pointer at element `index` of the array at a C pointer. The element's
//! VIR layout is C's (a C struct's aggregate is checked against C's rules where it is laid out,
//! layout.rs `check_c_struct_layout`), so the element is one load or store of its VIR type.
//!
//! - **Read**: C has no non-null pointer, so every non-null `Ptr<U>` in what arrives, the element
//!   itself or a C struct's field at any depth, is compared with 0, and the program stops on
//!   NULL, naming the field: with the null niche, a non-null `Ptr<U>` holding 0 would read as
//!   `null` once widened. A struct with none has no check.
//! - **wasm32**: Velt's `usize`, `isize` and pointers take 8 bytes in memory (VIR's types are the
//!   same on every target), C's 4, so an element holding one has another layout there: a compile
//!   error at the user's call naming the target.

use velt_sema::hir::{self, IntTy, Nothing, TyId, TyKind};

use super::operand::proj;
use super::{cint, unit, Cx, FnLower};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// `ptrRead<T>(p, index)`, `ty` being `T`: the element, its non-null pointers checked.
    pub(super) fn ptr_read(&mut self, p: &hir::Expr, index: &hir::Expr, ty: TyId) -> Operand {
        let t = self.sub(ty);
        let vt = self.cx.ty(t);
        let p_op = self.expr(p);
        let i = self.expr_as_u64(index);
        if self.refused_on_wasm32("read", t, p.span) {
            return Operand::Copy(Place::local(self.temp(vt)));
        }
        let at = self.element_place(p_op, i, vt);
        let v = Place::local(self.temp(vt));
        self.assign(v.clone(), Rvalue::Use(Operand::Copy(at)));
        let name = self.cx.element_name(t);
        self.check_non_null(&v, t, &mut vec![], &name);
        Operand::Copy(v)
    }

    /// `ptrWrite<T>(p, index, v)`: `v` stored at the element.
    pub(super) fn ptr_write(&mut self, p: &hir::Expr, index: &hir::Expr, v: &hir::Expr) -> Operand {
        let t = self.sub(v.ty);
        let vt = self.cx.ty(t);
        let p_op = self.expr(p);
        let i = self.expr_as_u64(index);
        let value = self.expr(v);
        if self.refused_on_wasm32("write", t, p.span) {
            return unit();
        }
        let at = self.element_place(p_op, i, vt);
        self.assign(at, Rvalue::Use(value));
        unit()
    }

    /// The place of element `i` (a `u64`) of type `vt` of the array at pointer `p`: at
    /// `p + i * size`, a VIR size being a multiple of its alignment, as C's array stride is.
    fn element_place(&mut self, p: Operand, i: Operand, vt: Ty) -> Place {
        let (size, _) = self.cx.size_align(vt);
        let base = self.rvalue_temp(Ty::U64, Rvalue::Cast(p, Ty::U64));
        let off = self.u64_op(BinOp::Mul, i, cint(size as i128, Ty::U64));
        let addr = self.u64_op(BinOp::Add, base, off);
        let at = self.rvalue_temp(Ty::Ptr, Rvalue::Cast(addr, Ty::Ptr));
        proj(&self.operand_place(at, Ty::Ptr), Proj::Deref(vt))
    }

    /// Stops the program where a non-null C pointer in `v` (of type `t`, a C struct's fields at
    /// any depth, whose names so far are `path`) is NULL. `element` names the type read.
    fn check_non_null(&mut self, v: &Place, t: TyId, path: &mut Vec<String>, element: &str) {
        match self.cx.kind(t) {
            TyKind::Ptr(_) => {
                let word =
                    self.rvalue_temp(Ty::U64, Rvalue::Cast(Operand::Copy(v.clone()), Ty::U64));
                let null =
                    self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, word, cint(0, Ty::U64)));
                let (bad, ok) = (self.new_block(), self.new_block());
                self.branch(null, bad, ok);
                self.switch_to(bad);
                let msg = match path.is_empty() {
                    true => "Ptr.read: the pointer read is NULL, and a `Ptr<T>` is never NULL (read a `Ptr<T> | null`)".to_string(),
                    false => format!(
                        "Ptr.read: field `{}` of the `{element}` read is NULL, and a `Ptr<T>` is never NULL (declare the field `Ptr<T> | null`)",
                        path.join(".")
                    ),
                };
                self.panic_msg(&msg);
                self.terminate(vir::Terminator::Unreachable);
                self.switch_to(ok);
            }
            TyKind::Adt(d, _) if self.cx.c_struct(t) => {
                let fields = self.cx.adt_field_tys(t);
                for (i, f) in fields.into_iter().enumerate() {
                    if !self.cx.holds_non_null_ptr(f) {
                        continue;
                    }
                    let vf = self.cx.vir_field(t, i as u32);
                    path.push(self.cx.adt_def(d).fields[i].name.clone());
                    self.check_non_null(&proj(v, Proj::Field(vf)), f, path, element);
                    path.pop();
                }
            }
            _ => {}
        }
    }

    /// Reports `Ptr.read` / `Ptr.write` (`what`) of a `t` on wasm32 where its layout isn't C's,
    /// at the user's call (`at` is in std). True if it did.
    fn refused_on_wasm32(&mut self, what: &str, t: TyId, at: velt_common::Span) -> bool {
        if !super::c_call::wasm32(&self.cx.target) || !self.cx.wide_on_wasm32(t) {
            return false;
        }
        let at = self.cx.user_span(at);
        let message = format!(
            "`Ptr.{what}` of a `{}` is not available on {}",
            self.cx.element_name(t),
            self.cx.target
        );
        if !(self.cx.c_clashes.iter())
            .any(|d| d.message == message && d.labels.first().is_some_and(|l| l.span == at))
        {
            self.cx.c_clashes.push(
                velt_common::Diagnostic::error(message, at).with_note(
                    "its layout differs from C's there: Velt's `usize`, `isize` and pointers take 8 bytes in memory, and C's 4",
                ),
            );
        }
        true
    }
}

impl Cx<'_> {
    /// How Velt writes `t`, an element `Ptr.read` and `Ptr.write` move, for their messages.
    fn element_name(&mut self, t: TyId) -> String {
        match self.kind(t) {
            TyKind::Int(i) => match i {
                IntTy::I8 => "i8",
                IntTy::I16 => "i16",
                IntTy::I32 => "i32",
                IntTy::I64 => "i64",
                IntTy::ISize => "isize",
                IntTy::U8 => "u8",
                IntTy::U16 => "u16",
                IntTy::U32 => "u32",
                IntTy::U64 => "u64",
                IntTy::USize => "usize",
            }
            .into(),
            TyKind::Unit => "void".into(),
            TyKind::Ptr(p) => format!("Ptr<{}>", self.element_name(p)),
            TyKind::Option(inner, n) => {
                let nothing = match n {
                    Nothing::Null => "null",
                    Nothing::Undefined => "undefined",
                    Nothing::Both => "null | undefined",
                };
                format!("{} | {nothing}", self.element_name(inner))
            }
            _ => self.type_name(t),
        }
    }

    /// Does a value of `t` hold a non-null C pointer: is it one, or a C struct with one among
    /// its fields at any depth?
    pub(super) fn holds_non_null_ptr(&mut self, t: TyId) -> bool {
        match self.kind(t) {
            TyKind::Ptr(_) => true,
            _ if self.c_struct(t) => {
                let fields = self.adt_field_tys(t);
                fields.into_iter().any(|f| self.holds_non_null_ptr(f))
            }
            _ => false,
        }
    }

    /// Does `t` (an element `Ptr.read` and `Ptr.write` move) hold a `usize`, an `isize` or a C
    /// pointer, which take 8 bytes in Velt's memory and 4 in C's on wasm32?
    pub(super) fn wide_on_wasm32(&mut self, t: TyId) -> bool {
        match self.kind(t) {
            TyKind::Int(IntTy::USize | IntTy::ISize) | TyKind::Ptr(_) => true,
            TyKind::Option(inner, n) => n != Nothing::Both && self.wide_on_wasm32(inner),
            _ if self.c_struct(t) => {
                let fields = self.adt_field_tys(t);
                fields.into_iter().any(|f| self.wide_on_wasm32(f))
            }
            _ => false,
        }
    }
}
