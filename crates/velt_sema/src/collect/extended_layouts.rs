//! Object types extended with optional fields can share one layout (#1383): for
//! `interface InternalVNode extends VNode { _ns?: boolean }` where both are object types
//! (field-only interfaces, or recursive aliases checked as one), `vnode as InternalVNode` is the
//! same object, as in TypeScript, instead of a copy that a later `._ns = …` would not reach.
//!
//! Each type keeps its declared fields: `VNode` has no member `_ns`, `keyof VNode`, mapped types,
//! structural checks, printing and JSON see `VNode`'s own fields. Only the layout is shared, and
//! only when the program converts between the base and an extension (a cast, an argument, an
//! assignment, also inside an array, a `Map` or another container; [`Ctx::family_layout`]): then
//! [`crate::layout_merge`] gives the base's object type (its anonymous twin) room for the
//! extensions' fields and makes the extensions' twins that type. An extension the program only
//! declares costs nothing.
//!
//! A `VNode` made anywhere then has the added fields absent (an absent optional field is not
//! printed, serialized or listed, as Node's object would not have the property), and a write
//! through the extension's type reaches every view of the object.
//!
//! Only plain extensions qualify: the base and the extension are not generic and not in the
//! standard library, the extension names one base and declares only optional fields that the
//! base doesn't have, and it is not extended itself. Extensions of one base are taken nearest to
//! the base first (by module path: one in the base's own directory before one in another
//! package), then by module path and definition order, while they give a field of one name the
//! same type; one that disagrees with those taken keeps its own layout. Anything else keeps its
//! own layout too (and the conversion copies, checked by `crate::object_copies`).

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::Ctx;
use crate::defs::FieldInfo;
use crate::hir::{DefId, TyId, TyKind};

/// The layouts extensions may share (`Ctx::objects`).
#[derive(Default)]
pub(crate) struct Layouts {
    /// Base layout (the base's anonymous twin) -> its family.
    pub families: FxHashMap<DefId, Family>,
    /// An extension's anonymous twin -> the base layout it shares.
    pub redirect: FxHashMap<DefId, DefId>,
    /// Extensions that keep their own layout because they give a field of their base another
    /// type than an extension sharing the base's layout: object type -> (the field, the
    /// interface whose type it has there).
    pub conflicts: FxHashMap<DefId, (String, DefId)>,
    /// The program is checked: from here on only families already in use answer
    /// [`Ctx::family_layout`] (a check must not widen a layout that no conversion needs).
    pub sealed: bool,
    /// The concrete types `JSON.parse` builds (`json::check_json_types`): objects of a widened
    /// layout made from text may have any of its fields.
    pub parsed: Vec<TyId>,
}

/// One base layout and the fields its extensions add.
pub(crate) struct Family {
    /// The extensions' own fields, in order (each extension's twin's `FieldInfo`).
    pub added: Vec<FieldInfo>,
    /// Per added field, the object type of the extension that declares it.
    pub from: Vec<DefId>,
    /// The program converts between the base and an extension: the layout is widened.
    pub used: bool,
}

/// The interface and object type pairs of `Ctx::field_only` (interface, object type), their
/// anonymous twins made: find the families of plain extensions that may share their base's
/// layout. Nothing is widened here ([`crate::layout_merge`]).
pub(super) fn families(cx: &mut Ctx, pairs: &[(DefId, DefId)]) {
    let adt_of: FxHashMap<DefId, DefId> = pairs.iter().copied().collect();
    // Base interface -> its plain extensions (object type, own fields).
    let mut by_base: FxHashMap<DefId, Vec<(DefId, Vec<FieldInfo>)>> = FxHashMap::default();
    let mut order: Vec<DefId> = vec![];
    for &(iface, adt) in pairs {
        let Some((base, own)) = plain_extension(cx, iface, &adt_of) else {
            continue;
        };
        if !by_base.contains_key(&base) {
            order.push(base);
        }
        by_base.entry(base).or_default().push((adt, own));
    }
    order.sort_by_key(|d| d.0);
    let twin = |cx: &Ctx, adt: DefId| cx.readonly_twins.get(&adt).map(|t| t.0);
    let mut layouts = Layouts::default();
    for base in order {
        let members = &by_base[&base];
        // A type that is both an extension and an extended base keeps its layout (chains are
        // not merged), and so do its family's members.
        if by_base
            .values()
            .flatten()
            .any(|(adt, _)| cx.field_only_of[adt] == base)
            || (members.iter()).any(|(adt, _)| by_base.contains_key(&cx.field_only_of[adt]))
        {
            continue;
        }
        let Some(base_twin) = twin(cx, adt_of[&base]) else {
            continue;
        };
        let Some(mut fields) = cx.adt(adt_of[&base]).map(|a| a.fields.clone()) else {
            continue;
        };
        // Two bases of one shape are one layout: their extensions are one family.
        let family = layouts.families.entry(base_twin).or_insert(Family {
            added: vec![],
            from: vec![],
            used: false,
        });
        fields.extend(family.added.iter().cloned());
        // Nearest to the base first, so that the choice does not depend on the order the
        // modules load in (module docs).
        let base_path = module_path(cx, base);
        let mut members: Vec<&(DefId, Vec<FieldInfo>)> = members.iter().collect();
        members.sort_by_cached_key(|(adt, _)| {
            let path = module_path(cx, cx.field_only_of[adt]);
            let near = (path.split('/').zip(base_path.split('/')))
                .take_while(|(a, b)| a == b)
                .count();
            (std::cmp::Reverse(near), path, cx.field_only_of[adt].0)
        });
        // The extension each added field comes from.
        let mut from: Vec<(String, DefId)> = vec![];
        for (adt, own) in members {
            let clash = own.iter().find(|f| {
                (fields.iter().find(|g| g.name == f.name)).is_some_and(|g| !same_field(g, f))
            });
            if let Some(f) = clash {
                let owner = from.iter().find(|(n, _)| *n == f.name).map(|x| x.1);
                if let Some(owner) = owner {
                    (layouts.conflicts).insert(*adt, (f.name.clone(), owner));
                }
                continue;
            }
            let Some(ext_twin) = twin(cx, *adt) else {
                continue;
            };
            let iface = cx.field_only_of[adt];
            from.extend(own.iter().map(|f| (f.name.clone(), iface)));
            // The twin's fields: optional ones read as `T | undefined` there, with defaults.
            let twin_fields = cx.adt(ext_twin).map_or(vec![], |a| a.fields.clone());
            for f in own {
                if fields.iter().any(|g| g.name == f.name) {
                    continue;
                }
                fields.push(f.clone());
                if let Some(t) = twin_fields.iter().find(|g| g.name == f.name) {
                    family.added.push(t.clone());
                    family.from.push(*adt);
                }
            }
            layouts.redirect.insert(ext_twin, base_twin);
        }
    }
    // A layout that is a base's in one family and an extension's in another (or an extension's
    // of two bases) is in no family.
    let shared: FxHashSet<DefId> = (layouts.redirect.keys())
        .filter(|d| layouts.families.contains_key(d))
        .copied()
        .collect();
    if !shared.is_empty() {
        let dropped: FxHashSet<DefId> = (layouts.redirect.iter())
            .filter(|(k, _)| shared.contains(k))
            .map(|(_, v)| *v)
            .chain(shared.iter().copied())
            .collect();
        layouts
            .redirect
            .retain(|k, v| !dropped.contains(k) && !dropped.contains(v));
        layouts.families.retain(|k, _| !dropped.contains(k));
    }
    layouts
        .families
        .retain(|k, f| !f.added.is_empty() && layouts.redirect.values().any(|v| v == k));
    cx.objects.layouts = layouts;
}

impl Ctx<'_> {
    /// Are `a` and `b` one layout once the families of [`families`] that they mention share
    /// their base's layout (`VNode` and `InternalVNode`, `VNode[]` and `InternalVNode[]`)? If
    /// so, those families are in use: lowering sees the widened layout ([`crate::layout_merge`]).
    pub(crate) fn family_layout(&mut self, a: TyId, b: TyId) -> bool {
        if a == b || self.objects.layouts.families.is_empty() {
            return false;
        }
        if !self.in_family(a) || !self.in_family(b) {
            return false;
        }
        let mut twins = self.readonly_twins.clone();
        redirect_twins(&mut twins, &self.objects.layouts.redirect);
        let mut cache = FxHashMap::default();
        let ea = crate::readonly::erase_ty(&mut self.ty, &twins, &mut cache, a);
        let eb = crate::readonly::erase_ty(&mut self.ty, &twins, &mut cache, b);
        if self.canon(ea) != self.canon(eb) {
            return false;
        }
        let mut found = vec![];
        self.family_defs(ea, &mut found);
        let sealed = self.objects.layouts.sealed;
        let families = &mut self.objects.layouts.families;
        if sealed
            && found
                .iter()
                .any(|d| families.get(d).is_some_and(|f| !f.used))
        {
            return false;
        }
        for d in found {
            if let Some(f) = families.get_mut(&d) {
                f.used = true;
            }
        }
        true
    }

    /// Does type `t` mention an object type of a family (a base, an extension, or a type of
    /// either's layout)?
    fn in_family(&self, t: TyId) -> bool {
        let l = &self.objects.layouts;
        let mut hit = false;
        self.ty.visit(t, &mut |k| {
            if let TyKind::Adt(d, _) = k {
                let twin = self.readonly_twins.get(d).map_or(*d, |t| t.0);
                hit |= l.families.contains_key(&twin) || l.redirect.contains_key(&twin);
            }
        });
        hit
    }

    /// The base layouts erased type `t` mentions.
    fn family_defs(&self, t: TyId, out: &mut Vec<DefId>) {
        self.ty.visit(t, &mut |k| {
            if let TyKind::Adt(d, _) = k {
                if self.objects.layouts.families.contains_key(d) && !out.contains(d) {
                    out.push(*d);
                }
            }
        });
    }

    /// `d` with a note when `target` is an extension that keeps its own layout because another
    /// extension of its base gives one of its fields another type.
    pub(crate) fn with_layout_note(
        &mut self,
        d: velt_common::Diagnostic,
        target: TyId,
    ) -> velt_common::Diagnostic {
        let TyKind::Adt(adt, _) = *self.ty.kind(target) else {
            return d;
        };
        let Some((field, other)) = self.objects.layouts.conflicts.get(&adt).cloned() else {
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
            true => format!(
                "the `{}` of `{}`",
                o.name,
                short_path(&self.modules[o.module].path)
            ),
            false => format!("`{}`", o.name),
        };
        let base = self
            .iface(base.iface)
            .map_or(String::new(), |b| b.name.clone());
        d.with_note(format!(
            "`{name}` extends `{base}` with a field `{field}` of another type than {other} gives it; `{base}`'s objects can have room for that one's fields only, so `{name}` is a separate object type"
        ))
        .with_note(format!(
            "give `{field}` the same type in both extensions, and a `{base}` converts to either as the same object"
        ))
    }
}

/// `twins` with each extension's layout replaced by its base's ([`Layouts::redirect`]).
impl Ctx<'_> {
    /// The fields the layout of object type `d` gets from the extensions of its family, when the
    /// program converts in that family (module docs): what an object of type `d` may hold
    /// besides its declared fields (each optional, absent unless written through an extension),
    /// each with the object type of the extension that declares it, through which it is read.
    pub(crate) fn layout_added_fields(&self, d: DefId) -> Vec<(FieldInfo, DefId)> {
        let layouts = &self.objects.layouts;
        if layouts.families.is_empty() {
            return vec![];
        }
        let twin = self.readonly_twins.get(&d).map_or(d, |t| t.0);
        let base = layouts.redirect.get(&twin).copied().unwrap_or(twin);
        let Some(family) = layouts.families.get(&base).filter(|f| f.used) else {
            return vec![];
        };
        let own: Vec<&str> = self
            .adt(d)
            .map_or(vec![], |a| a.fields.iter().map(|f| f.name.as_str()).collect());
        (family.added.iter().zip(&family.from))
            .filter(|(f, _)| !own.contains(&f.name.as_str()))
            .map(|(f, e)| (f.clone(), *e))
            .collect()
    }
}

pub(crate) fn redirect_twins(
    twins: &mut FxHashMap<DefId, (DefId, Option<Vec<TyId>>)>,
    redirect: &FxHashMap<DefId, DefId>,
) {
    for t in twins.values_mut() {
        if let Some(&d) = redirect.get(&t.0) {
            t.0 = d;
        }
    }
    for (&ext, &base) in redirect {
        twins.insert(ext, (base, None));
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

/// A module path as a note names it: its last three parts (`runtime-core/src/renderer-types`).
fn short_path(path: &str) -> String {
    let parts: Vec<&str> = (path.split('/'))
        .filter(|p| !p.is_empty() && *p != "..")
        .collect();
    parts[parts.len().saturating_sub(3)..].join("/")
}

/// Two extensions' fields of one name: the same field of the shared layout?
fn same_field(a: &FieldInfo, b: &FieldInfo) -> bool {
    a.declared == b.declared && a.optional == b.optional && a.readonly == b.readonly
}
