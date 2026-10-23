//! Object types extended with optional fields share one layout (#1383): for
//! `interface InternalVNode extends VNode { _ns?: boolean }` where both are object types
//! (field-only interfaces, or recursive aliases checked as one), `VNode`'s layout also holds the
//! extension's optional fields, so `vnode as InternalVNode` is the same object, as in
//! TypeScript, instead of a copy that a later `._ns = …` would not reach.
//!
//! A `VNode` made anywhere has the extra fields absent (an absent optional field is not
//! printed, serialized, spread or listed, as Node's object would not have the property), and a
//! write through the extension's type reaches every view of the object. The extension's type
//! gets the same fields (also those another extension of the same base adds), so the two
//! types have one layout ([`Ctx::same_layout`]) and convert to each other as an `Upcast`.
//!
//! Only plain extensions qualify: the base and the extension are not generic, the extension
//! names one base and declares only optional fields that the base doesn't have. Extensions of one
//! base are taken in definition order while they give a field of one name the same type; one
//! that disagrees with those taken keeps its own layout. Anything else keeps its own layout
//! too (and the conversion copies, checked by `crate::object_copies`).

use rustc_hash::FxHashMap;

use crate::ctx::Ctx;
use crate::defs::FieldInfo;
use crate::hir::DefId;

/// The interface and object type pairs of `Ctx::field_only` (interface, object type): give
/// each base that plain extensions extend their optional fields, and each extension the
/// base's resulting fields. Runs after the object types' own fields are filled, before their
/// layouts (anonymous twins) are made.
pub(super) fn widen(cx: &mut Ctx, pairs: &[(DefId, DefId)]) {
    let adt_of: FxHashMap<DefId, DefId> = pairs.iter().copied().collect();
    // Base interface -> its plain extensions (object type, own fields).
    let mut families: FxHashMap<DefId, Vec<(DefId, Vec<FieldInfo>)>> = FxHashMap::default();
    let mut order: Vec<DefId> = vec![];
    for &(iface, adt) in pairs {
        let Some((base, own)) = plain_extension(cx, iface, &adt_of) else {
            continue;
        };
        if !families.contains_key(&base) {
            order.push(base);
        }
        families.entry(base).or_default().push((adt, own));
    }
    // A type that is both an extension and an extended base keeps its layout (chains are not
    // merged), and so do its family's members.
    let extensions: Vec<DefId> = families
        .values()
        .flatten()
        .filter_map(|(adt, _)| cx.field_only_of.get(adt).copied())
        .collect();
    for base in order {
        if extensions.contains(&base) {
            continue;
        }
        let members = &families[&base];
        if members
            .iter()
            .any(|(adt, _)| families.contains_key(&cx.field_only_of[adt]))
        {
            continue;
        }
        let base_adt = adt_of[&base];
        let Some(mut fields) = cx.adt(base_adt).map(|a| a.fields.clone()) else {
            continue;
        };
        // Each extension whose fields agree with those taken so far, nearest to the base first
        // (one in the base's own directory before one in another package), so the choice does
        // not depend on the order the modules load in; then in definition order. The fields
        // come in that order too, after the base's.
        let base_path = module_path(cx, base);
        let mut members: Vec<&(DefId, Vec<FieldInfo>)> = members.iter().collect();
        members.sort_by_cached_key(|(adt, _)| {
            let path = module_path(cx, cx.field_only_of[adt]);
            let near = path
                .split('/')
                .zip(base_path.split('/'))
                .take_while(|(a, b)| a == b)
                .count();
            (std::cmp::Reverse(near), path, cx.field_only_of[adt].0)
        });
        let mut taken: Vec<DefId> = vec![];
        // The extension each added field comes from.
        let mut from: Vec<(String, DefId)> = vec![];
        for (adt, own) in members {
            let clash = own.iter().find(|f| {
                (fields.iter().find(|g| g.name == f.name)).is_some_and(|g| !same_field(g, f))
            });
            if let Some(f) = clash {
                let owner = from.iter().find(|(n, _)| *n == f.name).map(|x| x.1);
                if let Some(owner) = owner {
                    (cx.objects.layout_conflicts).insert(*adt, (f.name.clone(), owner));
                }
                continue;
            }
            let iface = cx.field_only_of[adt];
            from.extend(own.iter().map(|f| (f.name.clone(), iface)));
            for f in own {
                if !fields.iter().any(|g| g.name == f.name) {
                    fields.push(f.clone());
                }
            }
            taken.push(*adt);
        }
        let members = taken;
        cx.adt_mut(base_adt).fields = fields.clone();
        for adt in members {
            cx.adt_mut(adt).fields = fields.clone();
        }
    }
}

impl Ctx<'_> {
    /// `d` with a note when `target` is an extension that keeps its own layout because another
    /// extension of its base gives one of its fields another type.
    pub(crate) fn with_layout_note(
        &mut self,
        d: velt_common::Diagnostic,
        target: crate::hir::TyId,
    ) -> velt_common::Diagnostic {
        let crate::hir::TyKind::Adt(adt, _) = *self.ty.kind(target) else {
            return d;
        };
        let Some((field, other)) = self.objects.layout_conflicts.get(&adt).cloned() else {
            return d;
        };
        let (Some(me), Some(o)) = (self.field_only_of.get(&adt), self.iface(other)) else {
            return d;
        };
        let Some(base) = self.iface(*me).and_then(|i| i.parents.first()) else {
            return d;
        };
        let name = self.display(target);
        // Two extensions of one name in different modules: say which.
        let other = match o.name == name {
            true => format!("the `{}` of `{}`", o.name, self.modules[o.module].path),
            false => format!("`{}`", o.name),
        };
        let base = self
            .iface(base.iface)
            .map_or(String::new(), |b| b.name.clone());
        d.with_note(format!(
            "`{name}` extends `{base}` with a field `{field}` of another type than {other} gives it; `{base}`'s objects have room for that one's fields, so `{name}` is a separate object type"
        ))
        .with_note(format!(
            "give `{field}` the same type in both extensions, and a `{base}` converts to either as the same object"
        ))
    }
}

/// Is interface `iface` a plain extension (module docs)? Its base interface and own fields.
fn plain_extension(
    cx: &Ctx,
    iface: DefId,
    adt_of: &FxHashMap<DefId, DefId>,
) -> Option<(DefId, Vec<FieldInfo>)> {
    let i = cx.iface(iface)?;
    let decl = i.decl?;
    if cx.scopes[i.module].is_std || i.generics.len() != 0 || decl.extends.len() != 1 {
        return None;
    }
    let [parent] = i.parents.as_slice() else {
        return None;
    };
    let base = cx.iface(parent.iface)?;
    if !parent.args.is_empty()
        || base.generics.len() != 0
        || cx.scopes[base.module].is_std
        || !adt_of.contains_key(&parent.iface)
    {
        return None;
    }
    let own_n = decl.fields.len().min(i.fields.len());
    let own = &i.fields[..own_n];
    // Only fields of its own, all optional; the inherited ones are exactly the base's.
    if own_n == 0
        || own.iter().any(|f| !f.optional)
        || i.fields.len() != own_n + base.fields.len()
        || own
            .iter()
            .any(|f| base.fields.iter().any(|g| g.name == f.name))
    {
        return None;
    }
    Some((parent.iface, own.to_vec()))
}

/// The path of the module interface `iface` is declared in.
fn module_path(cx: &Ctx, iface: DefId) -> String {
    cx.iface(iface)
        .map_or_else(String::new, |i| cx.modules[i.module].path.clone())
}

/// Two extensions' fields of one name: the same field of the shared layout?
fn same_field(a: &FieldInfo, b: &FieldInfo) -> bool {
    a.declared == b.declared && a.optional == b.optional && a.readonly == b.readonly
}
