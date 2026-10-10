//! Field-only interfaces are object types (docs/internals/design/shared-models.md, step 2).
//!
//! An interface that declares at least one field, no methods, getters or setters, and extends
//! only field-only interfaces describes data. It gets a synthesized struct definition with its
//! fields ([`Ctx::field_only`]); the interface's name, written as a type, means that object type
//! (`crate::resolve`), so literals fill it, field reads are loads and it has a JSON form. As a
//! bound, in `extends` and in `implements`, the name still means the interface: classes that
//! declare `implements` get their usual getter impls, and other types satisfy a field-only bound
//! structurally (`Ctx::field_only_impl`). Before lowering, the object type is replaced by the
//! anonymous object type of its fields (`crate::readonly`), so it converts to and from object
//! types of the same layout (an `Upcast`) and stays the same object.

use std::collections::HashMap;

use velt_syntax::ast;

use crate::ctx::{Ctx, Item};
use crate::defs::{AdtInfo, DefInfo, Generics};
use crate::hir::{AdtKind, DefId, TyKind};

/// After declaration (names resolvable, no type resolved yet): find the field-only interfaces
/// and give each an object type definition (its fields come in [`fill`]).
pub(super) fn declare(cx: &mut Ctx) {
    let ifaces: Vec<DefId> = (0..cx.info.len() as u32)
        .map(DefId)
        .filter(|d| matches!(&cx.info[d.0 as usize], DefInfo::Iface(i) if i.decl.is_some()))
        .collect();
    let mut memo: HashMap<DefId, bool> = HashMap::new();
    for d in &ifaces {
        is_field_only(cx, *d, &mut memo, &mut vec![]);
    }
    for d in ifaces {
        if memo.get(&d) != Some(&true) {
            continue;
        }
        let DefInfo::Iface(i) = &cx.info[d.0 as usize] else {
            unreachable!("ICE: iface")
        };
        let info = AdtInfo {
            name: i.name.clone(),
            qual_name: i.qual_name.clone(),
            kind: AdtKind::Struct,
            module: i.module,
            span: i.span,
            generics: Generics {
                names: i.generics.names.clone(),
                bounds: vec![],
                defaults: vec![],
            },
            fields: vec![],
            own_fields_start: 0,
            base: None,
            methods: HashMap::new(),
            ctor: None,
            own_ctor: None,
            vtable: vec![],
            vslots: HashMap::new(),
            implements: vec![],
            has_dispose: false,
            statics: HashMap::new(),
            decl: None,
        };
        let span = i.span;
        let adt = cx.alloc_def(span, DefInfo::Adt(Box::new(info)));
        cx.field_only.insert(d, adt);
        cx.field_only_of.insert(adt, d);
    }
}

/// Whether interface `d` is field-only: own fields and no own methods, and every `extends`
/// names a field-only interface (a name that can't be followed here makes it nominal).
fn is_field_only(
    cx: &Ctx,
    d: DefId,
    memo: &mut HashMap<DefId, bool>,
    stack: &mut Vec<DefId>,
) -> bool {
    if let Some(&known) = memo.get(&d) {
        return known;
    }
    if stack.contains(&d) {
        return false; // an `extends` cycle is reported elsewhere
    }
    let Some(i) = cx.iface(d) else {
        return false;
    };
    let Some(decl) = i.decl else {
        return false;
    };
    let module = i.module;
    stack.push(d);
    let own = decl.methods.is_empty();
    let mut has_fields = !decl.fields.is_empty();
    let mut parents_ok = true;
    for t in &decl.extends {
        match parent_iface(cx, module, t) {
            Some(p) if is_field_only(cx, p, memo, stack) => has_fields = true,
            _ => parents_ok = false,
        }
    }
    stack.pop();
    let field_only = own && parents_ok && has_fields;
    memo.insert(d, field_only);
    field_only
}

/// The interface an `extends` entry names, when it is a plain (one-segment) name.
fn parent_iface(cx: &Ctx, module: usize, t: &ast::TypeExpr) -> Option<DefId> {
    let ast::TypeExprKind::Named { path, .. } = &t.kind else {
        return None;
    };
    let [name] = path.as_slice() else {
        return None;
    };
    match cx.lookup_item_at(module, &name.name, name.span)? {
        Item::Def(p) if cx.iface(p).is_some() => Some(p),
        _ => None,
    }
}

/// After interface inheritance is flattened: the object type's fields are the interface's,
/// inherited ones first (as a subclass lists its base class's fields first), with the
/// interface's generic bounds. It may contain itself (`next?: Node`): lowering stores such a
/// type behind a pointer (#376).
pub(super) fn fill(cx: &mut Ctx) {
    let pairs: Vec<(DefId, DefId)> = cx.field_only.iter().map(|(i, a)| (*i, *a)).collect();
    for (iface, adt) in &pairs {
        let i = cx.iface(*iface).expect("ICE: iface");
        let own = i.decl.map_or(0, |d| d.fields.len()).min(i.fields.len());
        let mut fields = i.fields[own..].to_vec();
        fields.extend(i.fields[..own].iter().cloned());
        let bounds = i.generics.bounds.clone();
        let a = cx.adt_mut(*adt);
        a.fields = fields;
        a.generics.bounds = bounds;
    }
    // The anonymous object type of the same fields replaces it before lowering.
    for (_, adt) in pairs {
        let a = cx.adt(adt).expect("ICE: adt");
        let module = a.module;
        let fields: Vec<crate::anon::ShapeField> =
            a.fields.iter().map(crate::anon::shape_field).collect();
        let (anon, template) = cx.anon_def_with(&fields, module);
        let (twin, template) = match cx.readonly_twins.get(&anon) {
            Some((plain, None)) => (*plain, template),
            _ => (anon, template),
        };
        cx.readonly_twins.insert(adt, (twin, Some(template)));
    }
}

impl Ctx<'_> {
    /// `t` satisfies field-only interface `iface` structurally: it has fields of the same names
    /// and types (a struct, class or object type, not generic, public fields). The getter impl
    /// that `T extends iface` code reads through is synthesized here, once per type. Generic
    /// field-only interfaces and generic types are satisfied only through `implements`.
    pub fn field_only_impl(
        &mut self,
        t: crate::hir::TyId,
        iface: DefId,
    ) -> Option<(u32, Vec<crate::hir::TyId>, crate::hir::TyId)> {
        let i = self.iface(iface)?;
        if i.generics.len() != 0 {
            return None;
        }
        let want: Vec<(String, crate::hir::TyId)> =
            i.fields.iter().map(|f| (f.name.clone(), f.ty)).collect();
        let TyKind::Adt(d, _) = self.ty.kind(t).clone() else {
            return None;
        };
        let a = self.adt(d)?;
        if a.generics.len() != 0 {
            return None;
        }
        let mut found = vec![];
        for (name, ty) in &want {
            let (index, f) = a
                .fields
                .iter()
                .enumerate()
                .find(|(_, f)| f.name == *name && f.private_to.is_none())?;
            if f.ty != *ty {
                return None;
            }
            found.push((index, *ty));
        }
        let methods = found
            .into_iter()
            .map(|(index, ty)| super::getters::field_getter(self, d, t, index, ty))
            .collect();
        self.impls.push(crate::hir::ImplDef {
            ty: t,
            generics: 0,
            iface,
            iface_args: vec![],
            methods,
        });
        Some(((self.impls.len() - 1) as u32, vec![], t))
    }
}
