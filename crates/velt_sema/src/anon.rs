//! Anonymous object types (`{ a: 1 }` literals, `{ kind: "circle"; readonly r: f64 }` written
//! types): one synthesized `AdtKind::Anon` def per shape (field names, types and `readonly` flags,
//! in order), generic over the type parameters its fields mention (renumbered by first
//! occurrence). Shapes that differ only in `readonly` convert to each other
//! ([`Ctx::same_layout`]).

use std::collections::HashMap;

use velt_common::Span;

use crate::ctx::Ctx;
use crate::defs::{AdtInfo, DefInfo, FieldInfo, Generics};
use crate::hir::{AdtKind, DefId, TyId, TyKind};

impl Ctx<'_> {
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

    /// Whether `a` and `b` are anonymous object types that are one type for lowering
    /// (`crate::readonly`): the same fields, differing at most in `readonly`. A value converts
    /// from one to the other and stays the same object. Equal fields alone are not enough: a
    /// generic alias's instance (`Box<number>`) and the concrete type (`{ v: number }`) are
    /// different definitions after erasure, so converting between them would be a type
    /// mismatch in lowering.
    pub fn same_layout(&mut self, a: TyId, b: TyId) -> bool {
        let anon = |cx: &Self, t: TyId| match cx.ty.kind(t) {
            TyKind::Adt(d, _) => cx.adt(*d).is_some_and(|x| x.kind == AdtKind::Anon),
            _ => false,
        };
        if a == b || !anon(self, a) || !anon(self, b) || self.readonly_twins.is_empty() {
            return false;
        }
        let twins = self.readonly_twins.clone();
        let mut cache = HashMap::new();
        crate::readonly::erase_ty(&mut self.ty, &twins, &mut cache, a)
            == crate::readonly::erase_ty(&mut self.ty, &twins, &mut cache, b)
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
            self.readonly_twins.insert(d, twin);
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
