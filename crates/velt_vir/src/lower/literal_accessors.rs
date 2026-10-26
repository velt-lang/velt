//! The accessors of an object literal (#1538): the literal's struct holds a getter `get k()` as
//! a function in field `k` and a setter `set k(v)` in field `set k`, and sema lists them as the
//! struct's getters and setters (`Program::dyn_members`). Through `any` the accessor runs
//! (any_glue.rs, any_set.rs); here is how the fields show elsewhere, as in Node: `console.log`
//! prints `k: [Getter]`, `[Setter]` or `[Getter/Setter]` without running it (and no type name),
//! `Object.keys` and `for...in` list `k` once, and `JSON.stringify` writes what the getter
//! returns (a property with only a setter is left out).

use velt_sema::hir::{DefId, DynMemberKind, TyId, TyKind};

use super::FnLower;
use crate::vir::{Operand, Place, Ty};

/// A field of an object literal's struct that holds an accessor.
pub(super) struct AccessorField {
    /// The field's name (`value`, `set value`).
    pub field: String,
    /// The property it stands for (`value`); `None` for a setter next to its getter, which
    /// is no property of its own.
    pub prop: Option<String>,
    /// How `console.log` shows the property.
    pub label: &'static str,
    /// The field holds the getter.
    pub getter: bool,
}

impl FnLower<'_, '_> {
    /// The fields of struct `d` that hold an object literal's accessors (module docs).
    pub(super) fn accessor_props(&self, d: DefId) -> Vec<AccessorField> {
        let members = self.dyn_members_of(d);
        let has = |k: DynMemberKind, n: &str| members.iter().any(|m| m.kind == k && m.name == n);
        let fields = &self.cx.adt_def(d).fields;
        let field = |n: &str| fields.iter().any(|f| f.name == n);
        let mut out = vec![];
        for m in &members {
            match m.kind {
                DynMemberKind::Getter if field(&m.name) => out.push(AccessorField {
                    field: m.name.clone(),
                    prop: Some(m.name.clone()),
                    label: match has(DynMemberKind::Setter, &m.name) {
                        true => "[Getter/Setter]",
                        false => "[Getter]",
                    },
                    getter: true,
                }),
                DynMemberKind::Setter if field(&format!("set {}", m.name)) => {
                    let alone = !has(DynMemberKind::Getter, &m.name);
                    out.push(AccessorField {
                        field: format!("set {}", m.name),
                        prop: alone.then(|| m.name.clone()),
                        label: "[Setter]",
                        getter: false,
                    })
                }
                _ => {}
            }
        }
        out
    }

    /// Field `name` of `d` as a property: itself, the property an accessor field stands for,
    /// or `None` for a setter next to its getter.
    pub(super) fn field_prop(&self, d: DefId, name: &str) -> Option<String> {
        match self.accessor_props(d).into_iter().find(|a| a.field == name) {
            Some(a) => a.prop,
            None => Some(name.to_string()),
        }
    }

    /// How `console.log` shows field `index` of `ty` when it holds an accessor.
    pub(super) fn accessor_label(&self, ty: TyId, index: u32) -> Option<&'static str> {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return None;
        };
        let name = &self.cx.adt_def(d).fields.get(index as usize)?.name;
        let acc = self.accessor_props(d);
        acc.into_iter().find(|a| a.field == *name).map(|a| a.label)
    }

    /// Does struct `d` hold an object literal's accessors? Then it prints without a type name.
    pub(super) fn has_accessor_fields(&self, d: DefId) -> bool {
        !self.accessor_props(d).is_empty()
    }

    /// Does field `name` of `ty` hold an object literal's getter?
    pub(super) fn holds_literal_getter(&self, ty: TyId, name: &str) -> bool {
        let Some((d, _)) = self.plain_adt(ty) else {
            return false;
        };
        let acc = self.accessor_props(d);
        acc.iter().any(|a| a.field == name && a.getter)
    }

    /// Run the getter that field `name` of the value at `place` (of type `ty`) holds: its
    /// result in a new `any` (owned), or `None` when the field holds no getter.
    pub(super) fn run_literal_getter(
        &mut self,
        place: &Place,
        ty: TyId,
        name: &str,
    ) -> Option<Place> {
        let (d, _) = self.plain_adt(ty)?;
        let acc = self.accessor_props(d);
        acc.iter().find(|a| a.field == name && a.getter)?;
        let m = (self.dyn_members_of(d).into_iter())
            .find(|m| m.kind == DynMemberKind::Getter && m.name == name)?;
        let r = self.ready(ty, &m)?;
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let out = Place::local(self.temp(avt));
        let this = match self.cx.ty(ty) {
            Ty::Agg(_) => self.addr(place.clone()),
            _ => Operand::Copy(place.clone()),
        };
        self.any_invoke(&r, this, vec![], &out);
        Some(out)
    }
}
