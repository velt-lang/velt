//! JS `===` on non-primitive values (`Intrinsic::Same`, semantics stage 2): objects — class
//! instances, arrays, object types, interface and function values — compare by identity, the
//! rest part by part. A counted value's identity is its pointer; an uncounted object has exactly
//! one home (it has a single owner), so its identity is the address of the place holding it.
//! An object type that is copied when shared (never changed in place) would lose its identity
//! that way, so comparing one records a fact that makes it counted once it is shared (boxing/).
//! Interface and function values: see `ref_identity`.

use velt_sema::hir::{self, TyId, TyKind};

use super::operand::proj;
use super::{FnLower, Glue};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// `a === b` for the values of concrete type `ty` at two places (Bool operand).
    pub(super) fn same_values(&mut self, a: &Place, b: &Place, ty: TyId) -> Operand {
        if self.cx.is_object(ty) {
            // Function and interface values keep their identity (a code / data pointer) when
            // copied: no fact (one would box them).
            if !self.cx.is_fn_or_dyn(ty) {
                self.cx.note_identity(ty);
            }
            return self.identity(a, b, ty);
        }
        match self.cx.kind(ty) {
            TyKind::Option(_) | TyKind::Tuple(_) | TyKind::Result(..) => self.same_glue(a, b, ty),
            TyKind::Adt(d, _) if !self.cx.is_c_like_enum(d) => self.same_glue(a, b, ty),
            _ => self.eq_values(a, b, ty),
        }
    }

    fn same_glue(&mut self, a: &Place, b: &Place, ty: TyId) -> Operand {
        let (pa, pb) = (self.addr(a.clone()), self.addr(b.clone()));
        self.call_glue(Glue::Same, ty, vec![pa, pb])
    }

    /// Identity of two objects of type `ty`.
    fn identity(&mut self, a: &Place, b: &Place, ty: TyId) -> Operand {
        let (x, y) = match self.cx.ty(ty) {
            // Class objects and counted boxes: the pointer is the object.
            Ty::Ptr => (Operand::Copy(a.clone()), Operand::Copy(b.clone())),
            // Interface and function values: compared here, not through `eq_values`: inside
            // `same` glue (`same_mode`) that comes back to `same_values` for objects, without end.
            _ if self.cx.is_fn_or_dyn(ty) => return self.ref_identity(a, b, ty),
            // A uniquely owned inline object: the address of its one home.
            _ => (self.addr(a.clone()), self.addr(b.clone())),
        };
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, x, y))
    }

    /// Identity of two function or interface values: an interface value is its data pointer
    /// (a class object, or a counted box once interface values are compared: `make_dyn`); a
    /// function value is its code and its environment (each evaluation of a closure has its
    /// own once function values are compared: closure.rs).
    pub(super) fn ref_identity(&mut self, a: &Place, b: &Place, ty: TyId) -> Operand {
        if matches!(self.cx.kind(ty), TyKind::Dyn(..)) {
            self.cx.note_identity(ty);
            return self.word_eq(a, b, 0);
        }
        self.cx.note_fn_identity();
        let code = self.word_eq(a, b, 0);
        let env = self.word_eq(a, b, 1);
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, code, env))
    }

    /// Do the words `field` of the values at `a` and `b` agree (Bool operand)?
    fn word_eq(&mut self, a: &Place, b: &Place, field: u32) -> Operand {
        let f = Proj::Field(field);
        let (x, y) = (Operand::Copy(proj(a, f.clone())), Operand::Copy(proj(b, f)));
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, x, y))
    }

    /// `Glue::Same` of an option / union / tuple: the equality glue comparing objects inside it
    /// by identity.
    pub(super) fn same_body(&mut self, pa: crate::vir::Local, pb: crate::vir::Local, ty: TyId) {
        self.same_mode = true;
        self.eq_body(pa, pb, ty);
    }
}

impl super::Cx<'_> {
    /// Is `t` an object type (compared by identity)?
    pub(super) fn is_object(&self, t: TyId) -> bool {
        match self.types.kind(t) {
            TyKind::Array(_) | TyKind::Dyn(..) | TyKind::FnPtr { .. } | TyKind::Closure(_) => true,
            TyKind::Adt(d, _) => matches!(self.hir.def(*d), hir::Def::Adt(_)),
            _ => false,
        }
    }

    /// Is `t` an object type whose values a share copies unless it is counted (an array or an
    /// object type, not a class, interface or function value)?
    pub(super) fn copied_object(&self, t: TyId) -> bool {
        self.is_object(t) && !self.is_class(t) && !self.is_fn_or_dyn(t)
    }

    fn is_fn_or_dyn(&self, t: TyId) -> bool {
        matches!(
            self.types.kind(t),
            TyKind::Dyn(..) | TyKind::FnPtr { .. } | TyKind::Closure(_)
        )
    }
}
