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
    /// (cast, implementation) pairs done.
    done: rustc_hash::FxHashSet<(usize, u32)>,
    /// The (interface, object type) pairs reported: one error per type, at the first cast.
    reported: rustc_hash::FxHashSet<(DefId, TyId)>,
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

    /// Widen each object type with an implementation of an interface some cast asks to widen
    /// (module docs), once per cast and implementation.
    pub(crate) fn widen_converted(&mut self) {
        if self.object_impls.widen.asks.is_empty() {
            return;
        }
        let asks = self.object_impls.widen.asks.clone();
        for (k, a) in asks.iter().enumerate() {
            for j in 0..self.impls.len() as u32 {
                let imp = &self.impls[j as usize];
                if imp.iface != a.iface || imp.generics != 0 {
                    continue;
                }
                let ty = imp.ty;
                if !self.object_impls.widen.done.insert((k, j)) {
                    continue;
                }
                if !self.widen_one(ty, &a.fields)
                    && self.object_impls.widen.reported.insert((a.iface, ty))
                {
                    self.no_room(ty, &a.fields, &a.cast, a.span);
                }
            }
        }
    }

    /// Give the object type `ty` the `fields` it lacks; `false` when it can't have them.
    fn widen_one(&mut self, ty: TyId, fields: &[FieldInfo]) -> bool {
        // An adapter (an `I<A>` as an `I<B>`, `body/expr/iface_adapt.rs`) forwards to the value
        // it wraps, whose own type is widened through its implementation; the run-time check
        // looks through the adapter.
        // Its struct, named `<interface> adapter` (which no source can name, for the space), holds
        // the wrapped value in its one field `inner`; a rebuilt adapter's earlier struct keeps
        // its implementation but leaves `Ctx::iface_adapters`.
        let adapter = match self.ty.kind(ty) {
            crate::hir::TyKind::Adt(d, _) => self.adt(*d).is_some_and(|a| {
                a.name.ends_with(" adapter")
                    && a.fields.len() == 1
                    && a.fields[0].name == "inner"
                    && matches!(self.ty.kind(a.fields[0].ty), crate::hir::TyKind::Dyn(..))
            }),
            _ => false,
        };
        if adapter {
            return true;
        }
        // A literal interface value (an object literal using `this`) or a function implements
        // the interface itself and gets no fields.
        let crate::hir::TyKind::Adt(d, args) = self.ty.kind(ty).clone() else {
            return false;
        };
        let Some(a) = self.adt(d) else {
            return false;
        };
        let (kind, own) = (a.kind, a.fields.clone());
        let missing: Vec<&FieldInfo> = (fields.iter())
            .filter(|f| !own.iter().any(|g| g.name == f.name))
            .collect();
        if missing.is_empty() {
            return true;
        }
        // An object literal's type, or the struct of one written as an interface with methods
        // (`body/expr/iface_object.rs`) whose methods don't use `this` (a value, which has presence
        // flags; one using `this` is laid out as a class).
        let literal = kind == AdtKind::Struct && self.literal_structs.contains(&d);
        if !(kind == AdtKind::Anon || literal) || !args.is_empty() || self.layout_family_of(d) {
            return false;
        }
        self.object_impls
            .widen
            .widened
            .entry(d)
            .or_insert(own.len());
        for f in missing {
            let mut f = f.clone();
            f.private_to = None;
            self.adt_mut(d).fields.push(f);
        }
        true
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
    fn no_room(&mut self, ty: TyId, fields: &[FieldInfo], cast: &(String, String), span: Span) {
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
        let note = if hidden {
            format!(
                "an object literal written as a `{from}` whose methods use `this` has no {lacks}, and its object gets no fields it was made without; give the literal the type `{to}` where it is made (`const c: {to} = {{ … }}`)"
            )
        } else if self.class_of(ty).is_some() {
            format!(
                "class `{shown}` is converted to `{from}` and doesn't declare {lacks}: a class's objects get no fields it doesn't declare; declare {lacks} on `{shown}` (optional is fine)"
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
