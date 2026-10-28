//! Which conversions of a dynamic value to an object type lowering may make member by member
//! (velt_vir `any_object_fill.rs`): an object type with optional or literal-typed fields, and
//! a function with properties (`#call` first), built from the members of a value of another
//! type, which lowering reads through `any`.

use crate::body::FnCx;
use crate::hir::{TyId, TyKind};

impl FnCx<'_, '_> {
    /// Note that `exp`, the target of a conversion of a dynamic value, may be built from the
    /// members of a value of another type: lowering then has the member slots.
    pub(super) fn note_object_fill(&mut self, exp: TyId) {
        let TyKind::Adt(d, _) = self.cx.ty.kind(exp).clone() else {
            return;
        };
        let fields = self.cx.adt(d).map(|a| a.fields.clone()).unwrap_or_default();
        let fill = fields
            .iter()
            .any(|f| f.optional || f.name == "#call" || self.cx.lit_value(f.declared).is_some());
        if fill {
            self.cx.note_dyn_members();
        }
    }
}
