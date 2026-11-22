//! What kind of type a monomorphic type is, for lowering's choices: the definitions behind it
//! (struct, class, enum, function), whether it is a class, a C-like enum, a union or a C pointer,
//! and whether `T | null` of it is one word with null for none (the null niche, types.rs).

use velt_sema::hir::{self, AdtKind, DefId, TyId, TyKind};

use super::{ice, Cx};

impl<'h> Cx<'h> {
    pub(super) fn adt_def(&self, d: DefId) -> &'h hir::AdtDef {
        match self.hir.def(d) {
            hir::Def::Adt(a) => a,
            _ => ice("expected a struct/class definition"),
        }
    }

    pub(super) fn enum_def(&self, d: DefId) -> &'h hir::EnumDef {
        match self.hir.def(d) {
            hir::Def::Enum(e) => e,
            _ => ice("expected an enum definition"),
        }
    }

    pub(super) fn fn_def(&self, d: DefId) -> &'h hir::FnDef {
        match self.hir.def(d) {
            hir::Def::Fn(f) => f,
            _ => ice("expected a function definition"),
        }
    }

    /// The `[Symbol.dispose]()` drop hook of a struct/class definition.
    pub(super) fn dispose_of(&self, d: DefId) -> Option<DefId> {
        match self.hir.def(d) {
            hir::Def::Adt(a) => a.dispose,
            _ => None,
        }
    }

    /// Is `t` a class instance type?
    pub(super) fn is_class(&self, t: TyId) -> bool {
        match self.types.kind(self.ph_rep(t)) {
            TyKind::Adt(d, _) => {
                matches!(self.hir.def(*d), hir::Def::Adt(a) if a.kind == AdtKind::Class)
            }
            _ => false,
        }
    }

    /// Enum whose variants carry no payload: represented by its `i64` discriminant.
    pub(super) fn is_c_like_enum(&self, d: DefId) -> bool {
        self.enum_def(d)
            .variants
            .iter()
            .all(|v| v.payload.is_empty())
    }

    /// Is `t` a union type (a compiler-generated enum, `EnumDef::is_union`)?
    pub(super) fn is_union(&self, t: TyId) -> bool {
        match self.types.kind(self.ph_rep(t)) {
            TyKind::Adt(d, _) => matches!(self.hir.def(*d), hir::Def::Enum(e) if e.is_union),
            _ => false,
        }
    }

    /// Is `t` a C pointer, velt/ffi's `Ptr<T>` (#3094)?
    pub(super) fn c_pointer(&self, t: TyId) -> bool {
        let t = self.ph_rep(t);
        matches!(self.types.kind(t), TyKind::Ptr(_))
    }

    /// `Option<t>` uses the null pointer for none.
    pub(super) fn has_null_niche(&self, t: TyId) -> bool {
        let t = self.ph_rep(t);
        self.is_class(t)
            || self.boxed(t)
            || matches!(
                self.types.kind(t),
                TyKind::Shared(_) | TyKind::Symbol | TyKind::Ptr(_)
            )
    }
}
