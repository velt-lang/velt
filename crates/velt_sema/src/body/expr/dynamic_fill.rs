//! Which conversions of a dynamic value to an object type lowering may make member by member
//! (velt_vir `any_object_fill.rs`): an object type with optional or literal-typed fields, or
//! fields an object literal's own field types may differ from (`any`, `unknown` and `object`,
//! options and unions: `{ reason: 1 }` held as `unknown`, then converted to an interface with
//! `reason: unknown`, #2554), and a function with properties (`#call` first), built from the
//! members of a value of another type, which lowering reads through `any`.

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
        let fill = fields.iter().any(|f| {
            f.optional
                || f.name == "#call"
                || self.cx.lit_value(f.declared).is_some()
                || self.cx.dynamic(f.ty).is_some()
                || self.cx.ty.opt_payload(f.ty).is_some()
                || self.cx.union_def(f.ty).is_some()
        });
        if fill {
            self.cx.note_dyn_members();
        }
    }
}
