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

    /// Whether `a` and `b` are anonymous object types with the same fields (names, types and
    /// order), differing at most in `readonly`: the same layout, so one converts to the other.
    pub fn same_layout(&mut self, a: TyId, b: TyId) -> bool {
        let (TyKind::Adt(d, xs), TyKind::Adt(e, ys)) =
            (self.ty.kind(a).clone(), self.ty.kind(b).clone())
        else {
            return false;
        };
        let fields = |cx: &Self, d: DefId| {
            cx.adt(d)
                .filter(|x| x.kind == AdtKind::Anon)
                .map(|x| x.fields.clone())
        };
        let (Some(x), Some(y)) = (fields(self, d), fields(self, e)) else {
            return false;
        };
        x.len() == y.len()
            && x.iter().zip(&y).all(|(f, g)| {
                f.name == g.name && self.ty.subst(f.ty, &xs) == self.ty.subst(g.ty, &ys)
            })
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
            })
            .collect();
        self.adt_mut(d).fields = fields;
        d
    }
}
