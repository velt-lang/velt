//! Anonymous object types (`{ a: 1 }` literals, `{ kind: "circle"; readonly r: f64 }` written
//! types): one synthesized `AdtKind::Anon` def per shape (field names, types and `readonly` flags,
//! in order), generic over the type parameters its fields mention (renumbered by first
//! occurrence). Shapes that differ only in `readonly` convert to each other
//! ([`Ctx::same_layout`]).
//!
//! Substitution keeps types canonical: `{ a: U }` is the def `{ a: T0 }` applied to `[U]`, and
//! with `U = string` it must be the same type as a written `{ a: string }` (the def
//! `{ a: string }`, no arguments). [`Ctx::subst`] substitutes and then re-interns every
//! anonymous object type from its substituted fields ([`Ctx::canon`]). Both forms have the same
//! fields in the same order, so they lay out alike (`velt_vir` maps both to one type), and
//! `assigned_fields.rs` makes them agree on `AdtDef::assigned`.

use std::collections::HashMap;

use velt_common::Span;

use crate::ctx::Ctx;
use crate::defs::{AdtInfo, DefInfo, FieldInfo, Generics};
use crate::hir::{AdtKind, DefId, TyId, TyKind};

impl Ctx<'_> {
    /// Replace `Param(i)` by `args[i]` in `t`, keeping anonymous object types canonical.
    pub fn subst(&mut self, t: TyId, args: &[TyId]) -> TyId {
        let t = self.ty.subst(t, args);
        self.canon(t)
    }

    /// [`Types::subst_known`](crate::types::Types::subst_known), keeping anonymous object
    /// types canonical.
    pub fn subst_known(&mut self, t: TyId, slots: &[Option<TyId>]) -> TyId {
        let t = self.ty.subst_known(t, slots);
        self.canon(t)
    }

    /// `t` with every anonymous object type in its canonical form: the def of its substituted
    /// field list, as if written directly.
    pub fn canon(&mut self, t: TyId) -> TyId {
        self.canon_depth(t, 0)
    }

    fn canon_depth(&mut self, t: TyId, depth: u32) -> TyId {
        if let Some(&c) = self.canon_memo.get(&t) {
            return c;
        }
        // Anonymous types never contain themselves (type aliases can't refer to themselves),
        // so this only guards against an ICE elsewhere.
        if depth > 64 {
            return t;
        }
        let c = |cx: &mut Self, x: TyId| cx.canon_depth(x, depth + 1);
        let k = match self.ty.kind(t).clone() {
            TyKind::Adt(d, args) => TyKind::Adt(d, args.iter().map(|a| c(self, *a)).collect()),
            TyKind::Dyn(d, args) => TyKind::Dyn(d, args.iter().map(|a| c(self, *a)).collect()),
            TyKind::Array(e) => TyKind::Array(c(self, e)),
            TyKind::Map(a, b) => TyKind::Map(c(self, a), c(self, b)),
            TyKind::Tuple(ts) => TyKind::Tuple(ts.iter().map(|a| c(self, *a)).collect()),
            TyKind::Option(e) => TyKind::Option(c(self, e)),
            TyKind::Result(a, b) => TyKind::Result(c(self, a), c(self, b)),
            TyKind::Promise(v, e) => TyKind::Promise(c(self, v), c(self, e)),
            TyKind::Shared(e) => TyKind::Shared(c(self, e)),
            TyKind::FnPtr {
                params,
                ret,
                throws,
            } => TyKind::FnPtr {
                params: params.iter().map(|a| c(self, *a)).collect(),
                ret: c(self, ret),
                throws: c(self, throws),
            },
            _ => {
                self.canon_memo.insert(t, t);
                return t;
            }
        };
        let mut out = self.ty.intern(k);
        if let TyKind::Adt(d, args) = self.ty.kind(out).clone() {
            if let Some(r) = self.canon_anon(d, &args, depth) {
                out = r;
            }
        }
        self.canon_memo.insert(t, out);
        out
    }

    /// The canonical type of anonymous def `d` applied to (canonical) `args`, if `d` is an
    /// anonymous def and that is a different type.
    fn canon_anon(&mut self, d: DefId, args: &[TyId], depth: u32) -> Option<TyId> {
        // `Error` arguments (unknown slots of an expected type) stay as they are: a def over
        // error fields would only be noise, and errors match anything anyway.
        if args.is_empty() || args.iter().any(|a| self.ty.has_error(*a)) {
            return None;
        }
        let a = self.adt(d).filter(|a| a.kind == AdtKind::Anon)?;
        let module = a.module;
        let templ: Vec<(String, TyId, bool)> = a
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty, f.readonly))
            .collect();
        // `readonly` flags are part of the shape: `{ readonly v: T0 }` at `string` is
        // `{ readonly v: string }`, not the writable `{ v: string }`.
        let fields: Vec<(String, TyId, bool)> = templ
            .into_iter()
            .map(|(n, ft, ro)| {
                let ft = self.ty.subst(ft, args);
                (n, self.canon_depth(ft, depth + 1), ro)
            })
            .collect();
        let (d2, args2) = self.anon_def_with(&fields, module);
        if d2 == d && args2 == args {
            return None;
        }
        Some(self.ty.intern(TyKind::Adt(d2, args2)))
    }

    /// The anonymous object type with these fields (none readonly).
    pub fn anon_type(&mut self, fields: &[(String, TyId)], module: usize) -> TyId {
        let (d, args) = self.anon_def(fields, module);
        self.ty.intern(TyKind::Adt(d, args))
    }

    /// The anonymous object type with these fields and `readonly` flags.
    pub fn anon_type_with(&mut self, fields: &[(String, TyId, bool)], module: usize) -> TyId {
        let (d, args) = self.anon_def_with(fields, module);
        self.ty.intern(TyKind::Adt(d, args))
    }

    /// Whether `a` and `b` are object types (anonymous ones, or field-only interfaces') that are
    /// one type for lowering (`crate::readonly`): the same fields, differing at most in
    /// `readonly` or in being a field-only interface. A value converts from one to the other and
    /// stays the same object. Equal fields alone are not enough: a generic type's instance
    /// (`Box<number>`) and the object type it spells out (`{ v: number }`) are different
    /// definitions after erasure, so converting between them would be a type mismatch in
    /// lowering.
    pub fn same_layout(&mut self, a: TyId, b: TyId) -> bool {
        let object = |cx: &Self, t: TyId| match cx.ty.kind(t) {
            TyKind::Adt(d, _) => cx
                .adt(*d)
                .is_some_and(|x| x.kind == AdtKind::Anon || cx.field_only_of.contains_key(d)),
            _ => false,
        };
        if a == b || !object(self, a) || !object(self, b) || self.readonly_twins.is_empty() {
            return false;
        }
        let twins = self.readonly_twins.clone();
        let mut cache = HashMap::new();
        crate::readonly::erase_ty(&mut self.ty, &twins, &mut cache, a)
            == crate::readonly::erase_ty(&mut self.ty, &twins, &mut cache, b)

    }

    /// Are `a` and `b` anonymous defs with the same field names, in order?
    pub(crate) fn same_anon_shape(&self, a: DefId, b: DefId) -> bool {
        let names = |d: DefId| {
            self.adt(d)
                .filter(|x| x.kind == AdtKind::Anon)
                .map(|x| x.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>())
        };
        a != b && names(a).is_some_and(|n| Some(n) == names(b))
    }

    /// Field types of anonymous def `d`, over its own parameters.
    pub(crate) fn anon_field_tys(&self, d: DefId) -> Vec<TyId> {
        self.adt(d)
            .map(|a| a.fields.iter().map(|f| f.ty).collect())
            .unwrap_or_default()
    }

    /// Record where the fields of anonymous object type `t` are written (for editors), if no
    /// written object type of this shape was seen before.
    pub fn declare_anon_fields(&mut self, t: TyId, spans: &[Span]) {
        let TyKind::Adt(d, _) = self.ty.kind(t).clone() else {
            return;
        };
        let a = self.adt_mut(d);
        if a.fields.iter().all(|f| f.span == Span::DUMMY) && a.fields.len() == spans.len() {
            for (f, sp) in a.fields.iter_mut().zip(spans) {
                f.span = *sp;
            }
        }
    }

    /// The anonymous object def of this shape (no field readonly) and its type arguments.
    pub fn anon_def(&mut self, fields: &[(String, TyId)], module: usize) -> (DefId, Vec<TyId>) {
        let fields: Vec<(String, TyId, bool)> =
            fields.iter().map(|(n, t)| (n.clone(), *t, false)).collect();
        self.anon_def_with(&fields, module)
    }

    /// The anonymous object def of this shape (with `readonly` flags) and its type arguments.
    pub fn anon_def_with(
        &mut self,
        fields: &[(String, TyId, bool)],
        module: usize,
    ) -> (DefId, Vec<TyId>) {
        let mut params: Vec<u32> = vec![];
        for (_, t, _) in fields {
            crate::types::collect_params(&self.ty, *t, &mut params);
        }
        let new_tys: HashMap<u32, TyId> = params
            .iter()
            .enumerate()
            .map(|(i, p)| (*p, self.ty.param(i as u32)))
            .collect();
        let norm: Vec<(String, TyId, bool)> = fields
            .iter()
            .map(|(n, t, readonly)| {
                let t = self.ty.map(*t, &mut |k| match k {
                    TyKind::Param(i) => new_tys.get(i).copied(),
                    _ => None,
                });
                (n.clone(), t, *readonly)
            })
            .collect();
        let params: Vec<TyId> = params.iter().map(|p| self.ty.param(*p)).collect();
        if let Some(&d) = self.anon.get(&norm) {
            return (d, params);
        }
        let d = self.new_anon_def(&norm, params.len(), module);
        self.anon.insert(norm.clone(), d);
        if norm.iter().any(|(_, _, readonly)| *readonly) {
            let plain: Vec<(String, TyId, bool)> = norm
                .iter()
                .map(|(n, t, _)| (n.clone(), *t, false))
                .collect();
            let (twin, _) = self.anon_def_with(&plain, module);
            self.readonly_twins.insert(d, (twin, None));
        }
        (d, params)
    }

    /// A fresh anonymous object def with fields `norm` (over `n` type params).
    fn new_anon_def(&mut self, norm: &[(String, TyId, bool)], n: usize, module: usize) -> DefId {
        let name = {
            let parts: Vec<String> = norm
                .iter()
                .map(|(n, t, readonly)| {
                    let readonly = if *readonly { "readonly " } else { "" };
                    format!("{readonly}{n}: {}", self.display(*t))
                })
                .collect();
            format!("{{ {} }}", parts.join(", "))
        };
        let mut generics = Generics::default();
        for i in 0..n {
            generics.push(&format!("T{i}"));
        }
        let info = AdtInfo {
            name: name.clone(),
            qual_name: name,
            kind: AdtKind::Anon,
            module,
            span: Span::DUMMY,
            generics,
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
        let d = self.alloc_def(Span::DUMMY, DefInfo::Adt(Box::new(info)));
        let fields = norm
            .iter()
            .map(|(n, t, readonly)| FieldInfo {
                name: n.clone(),
                ty: *t,
                span: Span::DUMMY,
                readonly: *readonly,
                optional: false,
                has_default: false,
                default: None,
                default_throws: vec![],
                private_to: None,
                inferred_int: false,
            })
            .collect();
        self.adt_mut(d).fields = fields;
        d
    }
}
