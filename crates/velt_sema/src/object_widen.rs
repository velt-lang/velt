//! Object types widened for a cast to an interface that adds optional fields (#379, step A2 of
//! `docs/internals/design/casts-across-layouts.md`): `(ctx as InternalCtx).__name = …`, where
//! `InternalCtx` extends `Ctx` with `__name?: string` and `ctx` is an object made as a `Ctx`.
//! Node adds the property when it is written; a Velt object keeps the layout it was made with.
//!
//! So each object literal type the program converts to `Ctx` gets the fields the extension adds,
//! absent until written, after its own: it then implements `InternalCtx`, and the checked cast
//! (`body/expr/identity_eq.rs`) finds that implementation. A literal made before the widening is
//! given the fields absent (`crate::layout_merge`). An absent field is not printed, serialized or
//! listed, as Node's object has no such property until it is written.
//!
//! The fields are added once every body is checked (`object_ifaces::make_dynamic_object_impls`,
//! before the implementations of the extension are made), so `keyof`, spreads and the structural
//! checks see the declared fields. A class converted to `Ctx` gets no fields it doesn't declare
//! (its objects have one layout per class, #379's decision), nor does an object type of another
//! kind: the cast is a compile error naming the type and the field.

use rustc_hash::FxHashMap;
use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::FieldInfo;
use crate::hir::{AdtKind, DefId, TyId};

/// A cast asking for room: the interface cast from, the optional fields the target adds, the
/// two types as written (`Ctx`, `InternalCtx`) and where it is.
#[derive(Clone)]
pub(crate) struct Ask {
    iface: DefId,
    fields: Vec<FieldInfo>,
    cast: (String, String),
    span: Span,
}

/// The widenings the program's casts ask for, and the object types widened (`Ctx::object_impls`).
#[derive(Clone, Default)]
pub(crate) struct Widen {
    /// The casts asking for it.
    pub asks: Vec<Ask>,
    /// The object types widened -> how many fields they declare (the added ones follow).
    pub widened: FxHashMap<DefId, usize>,
    /// The object types widened -> the first cast's target (`InternalCtx`), for diagnostics.
    pub by: FxHashMap<DefId, String>,
    /// (cast, implementation) pairs done.
    done: rustc_hash::FxHashSet<(usize, u32)>,
    /// The (interface, object type) pairs reported: one error per type, at the first cast.
    reported: rustc_hash::FxHashSet<(DefId, TyId)>,
}

/// Why an object type gets no room for a cast's fields.
pub(crate) enum Refusal {
    /// A class, an object literal using `this`, or another type with one layout.
    NoRoom,
    /// It has (or another cast gave it) a field of this name of another type.
    Conflict(String),
    /// A generic object literal type of its shape is laid out as it.
    GenericShape,
    /// An object literal made in a generic function: laid out per instance of its type
    /// parameters.
    GenericInstance,
}

impl Ctx<'_> {
    /// A cast of an `iface` value to an interface adding the optional `fields` (module docs).
    pub(crate) fn note_iface_widen(
        &mut self,
        iface: DefId,
        fields: Vec<FieldInfo>,
        cast: (String, String),
        span: Span,
    ) {
        let ask = Ask {
            iface,
            fields,
            cast,
            span,
        };
        self.object_impls.widen.asks.push(ask);
    }

    /// The interfaces some cast asks to widen the objects of.
    pub(crate) fn widened_ifaces(&self) -> Vec<DefId> {
        self.object_impls
            .widen
            .asks
            .iter()
            .map(|a| a.iface)
            .collect()
    }

    /// Widen each object type with an implementation of an interface some cast asks to widen,
    /// or of one extending it (module docs), once per cast and implementation. Every class
    /// implementing one is refused, converted or not: a conversion may be made where the class is
    /// known only at lowering (a type parameter, `any`).
    pub(crate) fn widen_converted(&mut self) {
        if self.object_impls.widen.asks.is_empty() {
            return;
        }
        let asks = self.object_impls.widen.asks.clone();
        for (k, a) in asks.iter().enumerate() {
            for j in 0..self.impls.len() as u32 {
                let imp = &self.impls[j as usize];
                let (ty, iface, generics) = (imp.ty, imp.iface, imp.generics);
                if !self.extends_or_is(iface, a.iface, 0) {
                    continue;
                }
                if !self.object_impls.widen.done.insert((k, j)) {
                    continue;
                }
                let r = match generics {
                    0 => self.widen_one(ty, &a.fields),
                    _ if self.is_adapter(ty) => Ok(()),
                    _ if self.is_literal_type(ty) => Err(Refusal::GenericInstance),
                    _ => Err(Refusal::NoRoom),
                };
                if r.is_ok() {
                    if let crate::hir::TyKind::Adt(d, _) = *self.ty.kind(ty) {
                        if self.object_impls.widen.widened.contains_key(&d) {
                            let by = &mut self.object_impls.widen.by;
                            by.entry(d).or_insert_with(|| a.cast.1.clone());
                        }
                    }
                }
                if let Err(why) = r {
                    if self.object_impls.widen.reported.insert((a.iface, ty)) {
                        self.no_room(ty, &a.fields, &a.cast, a.span, why);
                    }
                }
            }
        }
    }

    /// Is interface `d` the interface `base`, or does it extend it?
    fn extends_or_is(&self, d: DefId, base: DefId, depth: u32) -> bool {
        d == base
            || depth < 64
                && self.iface(d).is_some_and(|i| {
                    (i.parents.iter()).any(|p| self.extends_or_is(p.iface, base, depth + 1))
                })
    }

    /// An adapter (an `I<A>` as an `I<B>`, `body/expr/iface_adapt.rs`) forwards to the value it
    /// wraps, whose own type is widened through its implementation; the run-time check looks
    /// through the adapter. Its struct, named `<interface> adapter` (which no source can name, for
    /// the space), holds the wrapped value in its one field `inner`; a rebuilt adapter's earlier
    /// struct keeps its implementation but leaves `Ctx::iface_adapters`.
    fn is_adapter(&self, ty: TyId) -> bool {
        match self.ty.kind(ty) {
            crate::hir::TyKind::Adt(d, _) => self.adt(*d).is_some_and(|a| {
                a.name.ends_with(" adapter")
                    && a.fields.len() == 1
                    && a.fields[0].name == "inner"
                    && matches!(self.ty.kind(a.fields[0].ty), crate::hir::TyKind::Dyn(..))
            }),
            _ => false,
        }
    }

    /// Give the object type `ty` the `fields` it lacks, or say why it can't have them.
    fn widen_one(&mut self, ty: TyId, fields: &[FieldInfo]) -> Result<(), Refusal> {
        if self.is_adapter(ty) {
            return Ok(());
        }
        // A literal interface value (an object literal using `this`) or a function implements
        // the interface itself and gets no fields.
        let crate::hir::TyKind::Adt(d, args) = self.ty.kind(ty).clone() else {
            return Err(Refusal::NoRoom);
        };
        let Some(a) = self.adt(d) else {
            return Err(Refusal::NoRoom);
        };
        let (kind, own) = (a.kind, a.fields.clone());
        // A field another cast added must have the type this one gives it: one slot can't hold
        // both. A field the type declares itself must be one the extension's implementation can
        // read as its field: of its type, of it without `undefined` (`__name: string` for
        // `__name?: string`), or one a plain assignment keeps.
        let declared = (self.object_impls.widen.widened.get(&d).copied()).unwrap_or(own.len());
        let module = a.module;
        for f in fields {
            let Some(i) = own.iter().position(|g| g.name == f.name) else {
                continue;
            };
            let g = own[i].ty;
            let fits = match i >= declared {
                true => self.same_ty(g, f.ty),
                false => {
                    self.same_ty(g, f.ty)
                        || self
                            .ty
                            .opt_payload(f.ty)
                            .is_some_and(|p| self.same_ty(g, p))
                        || self.assigns(g, f.ty, module).is_some()
                }
            };
            if !fits {
                return Err(Refusal::Conflict(f.name.clone()));
            }
        }
        let missing: Vec<&FieldInfo> = (fields.iter())
            .filter(|f| !own.iter().any(|g| g.name == f.name))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        // An object literal's type, or the struct of one written as an interface with methods
        // (`body/expr/iface_object.rs`) whose methods don't use `this` (a value, which has presence
        // flags; one using `this` is laid out as a class).
        let literal = kind == AdtKind::Struct && self.literal_structs.contains(&d);
        if (kind == AdtKind::Anon || literal) && !args.is_empty() {
            return Err(Refusal::GenericInstance);
        }
        if !(kind == AdtKind::Anon || literal) || self.layout_family_of(d) {
            return Err(Refusal::NoRoom);
        }
        // A generic object type of the same shape (`{ el: T; … }` from a generic function) is laid
        // out as this type at its instances: it would need the fields too.
        if kind == AdtKind::Anon && self.generic_shape_of(d) {
            return Err(Refusal::GenericShape);
        }
        self.object_impls
            .widen
            .widened
            .entry(d)
            .or_insert(own.len());
        let names: Vec<String> = missing.iter().map(|f| f.name.clone()).collect();
        for f in missing {
            let mut f = f.clone();
            f.private_to = None;
            self.adt_mut(d).fields.push(f);
        }
        match self.rebuild_object_impls(d, &names) {
            Some(name) => Err(Refusal::Conflict(name)),
            None => Ok(()),
        }
    }

    /// Is `ty` an object literal's type, or the struct of one written as an interface?
    fn is_literal_type(&self, ty: TyId) -> bool {
        match self.ty.kind(ty) {
            crate::hir::TyKind::Adt(d, _) => self
                .adt(*d)
                .is_some_and(|a| a.kind == AdtKind::Anon || self.literal_structs.contains(d)),
            _ => false,
        }
    }

    /// Is there a generic object literal type with the field names and optional flags of `d`?
    /// Its declared fields count (the shape `Ctx::anon` keeps it under), not those a widening
    /// added.
    fn generic_shape_of(&self, d: DefId) -> bool {
        let shape = |a: &crate::defs::AdtInfo, n: usize| -> Vec<(String, bool)> {
            (a.fields.iter().take(n))
                .map(|f| (f.name.clone(), f.optional))
                .collect()
        };
        let n = (self.object_impls.widen.widened.get(&d).copied()).unwrap_or(usize::MAX);
        let Some(mine) = self.adt(d).map(|a| shape(a, n)) else {
            return false;
        };
        (0..self.info.len() as u32).map(DefId).any(|g| {
            g != d
                && self.adt(g).is_some_and(|a| {
                    a.kind == AdtKind::Anon && a.generics.len() != 0 && shape(a, usize::MAX) == mine
                })
        })
    }

    /// Is `t` the struct of an object literal written as an interface, widened (module docs)? Its
    /// implementations of the extensions are made as an object literal type's are.
    pub(crate) fn widened_literal_struct(&self, t: TyId) -> bool {
        match self.ty.kind(t) {
            crate::hir::TyKind::Adt(d, a) => {
                a.is_empty()
                    && self.literal_structs.contains(d)
                    && self.object_impls.widen.widened.contains_key(d)
            }
            _ => false,
        }
    }

    /// Is `d` (or its readonly twin) in a family of extended layouts (`collect::extended_layouts`)?
    fn layout_family_of(&self, d: DefId) -> bool {
        let layouts = &self.objects.layouts;
        let twin = self.readonly_twins.get(&d).map_or(d, |t| t.0);
        layouts.families.contains_key(&twin) || layouts.redirect.contains_key(&twin)
    }

    /// The cast `cast` needs room for `fields` in objects of type `ty`, which can't have it.
    fn no_room(
        &mut self,
        ty: TyId,
        fields: &[FieldInfo],
        cast: &(String, String),
        span: Span,
        why: Refusal,
    ) {
        let (from, to) = cast;
        let lacks: Vec<String> = (fields.iter())
            .filter(|f| match self.ty.kind(ty) {
                crate::hir::TyKind::Adt(d, _) => self
                    .adt(*d)
                    .is_none_or(|a| !a.fields.iter().any(|g| g.name == f.name)),
                _ => true,
            })
            .map(|f| format!("`{}`", f.name))
            .collect();
        let lacks = lacks.join(", ");
        let shown = self.display(ty);
        // The struct of an object literal written as an interface with methods whose methods use
        // `this` (`body/expr/iface_object.rs`).
        let hidden = match self.ty.kind(ty) {
            crate::hir::TyKind::Adt(d, _) => self.literal_structs.contains(d),
            _ => false,
        };
        let note = if let Refusal::Conflict(name) = &why {
            format!(
                "`{shown}` is converted to `{from}` and has a field `{name}` of another type than `{to}` gives it (its own, or one another cast added): one field can't hold both"
            )
        } else if matches!(why, Refusal::GenericShape) {
            format!(
                "`{shown}` is converted to `{from}`, and a generic object literal of its shape is laid out as it, which would need {lacks} too; give the object a type of its own (an interface declaring {lacks}) where it is made"
            )
        } else if matches!(why, Refusal::GenericInstance) {
            format!(
                "an object literal made in a generic function is converted to `{from}`, and is laid out per instance of the function's type parameters, so it gets no fields it was made without; give the literal the type `{to}` (with the function's type parameters) where it is made"
            )
        } else if hidden {
            format!(
                "an object literal written as a `{from}` whose methods use `this` has no {lacks}, and its object gets no fields it was made without; give the literal the type `{to}` where it is made (`const c: {to} = {{ … }}`)"
            )
        } else if self.class_of(ty).is_some() {
            format!(
                "class `{shown}` implements `{from}` and doesn't declare {lacks}: a class's objects get no fields it doesn't declare; declare {lacks} on `{shown}` (optional is fine)"
            )
        } else {
            format!(
                "`{shown}` is converted to `{from}` and has no {lacks}, and its objects get no fields they were made without; declare {lacks} on it (optional is fine)"
            )
        };
        self.error(
            Diagnostic::error(format!("cannot cast `{from}` as `{to}`"), span).with_note(note),
        );
    }
}
