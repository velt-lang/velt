//! Discriminated unions (docs/reference/types.md "Discriminated unions"): a union whose members are
//! object types (anonymous object types, structs, classes) that all have a field of a literal
//! type — the *discriminant*, `kind` in `{ kind: "circle"; r: f64 } | { kind: "rect"; ... }`.
//! No extra encoding: the union is an ordinary union enum and the member's variant index is the
//! tag, so the discriminant field is zero-sized and `s.kind` is a function of the tag.
//! `switch (s.kind)`, `s.kind === "circle"` and object literals typed by the union select
//! variants by the discriminant's literal values.

use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::hir::{DefId, LitValue, TyId, TyKind};

impl Ctx<'_> {
    /// Field `name` of `t` as code in the body of class `owner` names it. An ES private name
    /// `#x` is the field `owner` declares; when `owner` declares a `#x` that is not a field of
    /// `t` (a getter, a method, or a field `t` lacks), it is none of `t`'s fields, since
    /// another class's `#x` is a different member. Elsewhere (no `owner`, or `owner` declares
    /// no `#x`) it is the first field of that name, which the privacy check then rejects.
    pub fn field_seen_from(
        &mut self,
        t: TyId,
        name: &str,
        owner: Option<DefId>,
    ) -> Option<(u32, TyId)> {
        if !name.starts_with(ast::PRIVATE_NAME_PREFIX) {
            return self.field_of(t, name);
        }
        let TyKind::Adt(d, args) = self.ty.kind(t).clone() else {
            return None;
        };
        let a = self.adt(d)?;
        if let Some(owner) = owner {
            let found = a
                .fields
                .iter()
                .enumerate()
                .find(|(_, f)| f.name == name && f.private_to == Some(owner));
            if let Some((i, f)) = found {
                let fty = f.ty;
                return Some((i as u32, self.ty.subst(fty, &args)));
            }
            if self.declares_private_name(owner, name) {
                return None;
            }
        }
        self.field_of(t, name)
    }

    /// Does class `class` itself declare a member named `#x` (`name`): a field, a method or
    /// an accessor?
    pub fn declares_private_name(&self, class: DefId, name: &str) -> bool {
        let Some(a) = self.adt(class) else {
            return false;
        };
        a.fields
            .iter()
            .any(|f| f.name == name && f.private_to == Some(class))
            || a.methods.contains_key(name)
            || a.methods.contains_key(&crate::defs::member_key(name, true))
    }

    /// Field `name` of a struct / class / anonymous object type `t`: (index, field type).
    pub fn field_of(&mut self, t: TyId, name: &str) -> Option<(u32, TyId)> {
        let TyKind::Adt(d, args) = self.ty.kind(t).clone() else {
            return None;
        };
        let a = self.adt(d)?;
        let (i, f) = a.fields.iter().enumerate().find(|(_, f)| f.name == name)?;
        let fty = f.ty;
        Some((i as u32, self.ty.subst(fty, &args)))
    }

    /// The literal value of field `prop` in each member of union `u` (variant order), if `prop`
    /// is a discriminant: every member has it with a literal type.
    pub fn discriminant_values(&mut self, u: TyId, prop: &str) -> Option<Vec<LitValue>> {
        // Each class's `#kind` is its own member: never a discriminant shared by a union.
        if prop.starts_with(ast::PRIVATE_NAME_PREFIX) {
            return None;
        }
        let members = self.union_members(u)?;
        let mut out = vec![];
        for m in members {
            let (_, fty) = self.field_of(m, prop)?;
            out.push(self.lit_value(fty)?);
        }
        Some(out)
    }
}
