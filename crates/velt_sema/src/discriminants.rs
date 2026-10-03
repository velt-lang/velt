//! Discriminated unions (docs/reference/types.md "Discriminated unions"): a union whose members are
//! object types (anonymous object types, structs, classes) that all have a field of a literal
//! type — the *discriminant*, `kind` in `{ kind: "circle"; r: f64 } | { kind: "rect"; ... }`.
//! No extra encoding: the union is an ordinary union enum and the member's variant index is the
//! tag, so the discriminant field is zero-sized and `s.kind` is a function of the tag.
//! `switch (s.kind)`, `s.kind === "circle"` and object literals typed by the union select
//! variants by the discriminant's literal values.

use crate::ctx::Ctx;
use crate::hir::{LitValue, TyId, TyKind};

impl Ctx<'_> {
    /// Field `name` of a struct / class / anonymous object type `t`: (index, field type).
    pub fn field_of(&mut self, t: TyId, name: &str) -> Option<(u32, TyId)> {
        let TyKind::Adt(d, args) = self.ty.kind(t).clone() else {
            return None;
        };
        let a = self.adt(d)?;
        let (i, f) = a.fields.iter().enumerate().find(|(_, f)| f.name == name)?;
        let fty = f.ty;
        Some((i as u32, self.subst(fty, &args)))
    }

    /// The literal value of field `prop` in each member of union `u` (variant order), if `prop`
    /// is a discriminant: every member has it with a literal type.
    pub fn discriminant_values(&mut self, u: TyId, prop: &str) -> Option<Vec<LitValue>> {
        let members = self.union_members(u)?;
        let mut out = vec![];
        for m in members {
            let (_, fty) = self.field_of(m, prop)?;
            out.push(self.lit_value(fty)?);
        }
        Some(out)
    }
}
