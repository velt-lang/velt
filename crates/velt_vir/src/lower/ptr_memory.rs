//! velt/ffi's `Ptr.read<S>` and `Ptr.write<S>` (`ptrRead`, `ptrWrite`, #3094): the `S` at
//! element `index` of a C pointer, `index` times `S`'s size past its address. `S` is a number,
//! `bool`, a C pointer or a C struct (sema's `c_memory.rs`), whose aggregate is C's layout
//! (`c_layout.rs`). A read checks the pointers in the value declared non-null, in nested C
//! structs too: C may have left one `NULL`, which a `Ptr<T>` never is (with the null niche it
//! would read as `null` once widened), so the program stops, naming the field.

use velt_sema::hir::{self, TyId, TyKind};

use super::operand::proj;
use super::{cint, unit, FnLower};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// `ptrRead<S>(p, index)`, `ty` being `S`.
    pub(super) fn ptr_read(&mut self, p: &hir::Expr, index: &hir::Expr, ty: TyId) -> Operand {
        let ty = self.sub(ty);
        let t = self.vty(ty);
        let at = self.ptr_element(p, index, t);
        let place = proj(&self.operand_place(at, Ty::Ptr), Proj::Deref(t));
        let v = self.rvalue_temp(t, Rvalue::Use(Operand::Copy(place)));
        let held = self.operand_place(v.clone(), t);
        let s = self.cx.type_name(ty);
        self.stop_on_null(&held, ty, &s, "");
        v
    }

    /// `ptrWrite<S>(p, index, s)`.
    pub(super) fn ptr_write(&mut self, p: &hir::Expr, index: &hir::Expr, s: &hir::Expr) -> Operand {
        let t = self.vty(s.ty);
        let at = self.ptr_element(p, index, t);
        let v = self.expr(s);
        let place = proj(&self.operand_place(at, Ty::Ptr), Proj::Deref(t));
        self.assign(place, Rvalue::Use(v));
        unit()
    }

    /// The address of element `index` of `p`, a pointer to `t`s.
    fn ptr_element(&mut self, p: &hir::Expr, index: &hir::Expr, t: Ty) -> Operand {
        let p = self.expr(p);
        let index = self.expr(index);
        let (size, _) = self.cx.size_align(t);
        let base = self.rvalue_temp(Ty::U64, Rvalue::Cast(p, Ty::U64));
        let offset = self.u64_op(BinOp::Mul, index, cint(size.into(), Ty::U64));
        let at = self.u64_op(BinOp::Add, base, offset);
        self.rvalue_temp(Ty::Ptr, Rvalue::Cast(at, Ty::Ptr))
    }

    /// Stops the program where a non-null pointer in the value at `place`, of type `ty`, is
    /// `NULL`: `ty` itself, or a field (of a field…) of C struct `s`, `path` being the fields
    /// above it.
    fn stop_on_null(&mut self, place: &Place, ty: TyId, s: &str, path: &str) {
        match self.kind(ty) {
            TyKind::Ptr(_) => {
                let word =
                    self.rvalue_temp(Ty::U64, Rvalue::Cast(Operand::Copy(place.clone()), Ty::U64));
                let null =
                    self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, word, cint(0, Ty::U64)));
                let (bad, ok) = (self.new_block(), self.new_block());
                self.branch(null, bad, ok);
                self.switch_to(bad);
                let msg = match path.is_empty() {
                    true => format!("Ptr.read: the `{s}` read is NULL, and a C pointer declared `Ptr<T>` is never NULL; read a `Ptr<T> | null` where C may leave it NULL"),
                    false => format!("Ptr.read: field `{path}` of C struct `{s}` is NULL, and it is declared `Ptr<T>`, which is never NULL; declare it `Ptr<T> | null` if C may leave it NULL"),
                };
                self.panic_msg(&msg);
                self.terminate(vir::Terminator::Unreachable);
                self.switch_to(ok);
            }
            TyKind::Adt(d, _) if self.cx.c_struct(ty) => {
                let fields: Vec<(String, TyId)> = match self.cx.hir.def(d) {
                    hir::Def::Adt(a) => a.fields.iter().map(|f| (f.name.clone(), f.ty)).collect(),
                    _ => return,
                };
                for (k, (name, f)) in fields.into_iter().enumerate() {
                    if !self.cx.holds_non_null_ptr(f) {
                        continue;
                    }
                    let at = proj(place, Proj::Field(self.cx.vir_field(ty, k as u32)));
                    let path = match path.is_empty() {
                        true => name,
                        false => format!("{path}.{name}"),
                    };
                    self.stop_on_null(&at, f, s, &path);
                }
            }
            _ => {}
        }
    }
}

impl super::Cx<'_> {
    /// Is `t` a non-null C pointer, or a C struct holding one (in a nested C struct too)?
    pub(super) fn holds_non_null_ptr(&self, t: TyId) -> bool {
        match self.kind(t) {
            TyKind::Ptr(_) => true,
            TyKind::Adt(d, _) if self.c_struct(t) => match self.hir.def(d) {
                hir::Def::Adt(a) => a.fields.iter().any(|f| self.holds_non_null_ptr(f.ty)),
                _ => false,
            },
            _ => false,
        }
    }
}
