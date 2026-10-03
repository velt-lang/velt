//! Anonymous object types (`{ a: 1 }` literals, `{ kind: "circle"; r: f64 }` written types):
//! one synthesized `AdtKind::Anon` def per shape (field names and types, in order), generic over
//! the type parameters its fields mention (renumbered by first occurrence).

use std::collections::HashMap;

use velt_common::Span;

use crate::ctx::Ctx;
use crate::defs::{AdtInfo, DefInfo, FieldInfo, Generics};
use crate::hir::{AdtKind, DefId, TyId, TyKind};

impl Ctx<'_> {
    /// The anonymous object type with these fields.
    pub fn anon_type(&mut self, fields: &[(String, TyId)], module: usize) -> TyId {
        let (d, args) = self.anon_def(fields, module);
        self.ty.intern(TyKind::Adt(d, args))
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

    /// The anonymous object def of this shape and its type arguments.
    pub fn anon_def(&mut self, fields: &[(String, TyId)], module: usize) -> (DefId, Vec<TyId>) {
        let mut params: Vec<u32> = vec![];
        for (_, t) in fields {
            crate::types::collect_params(&self.ty, *t, &mut params);
        }
        let new_tys: HashMap<u32, TyId> = params
            .iter()
            .enumerate()
            .map(|(i, p)| (*p, self.ty.param(i as u32)))
            .collect();
        let norm: Vec<(String, TyId)> = fields
            .iter()
            .map(|(n, t)| {
                let t = self.ty.map(*t, &mut |k| match k {
                    TyKind::Param(i) => new_tys.get(i).copied(),
                    _ => None,
                });
                (n.clone(), t)
            })
            .collect();
        let params: Vec<TyId> = params.iter().map(|p| self.ty.param(*p)).collect();
        if let Some(&d) = self.anon.get(&norm) {
            return (d, params);
        }
        let d = self.new_anon_def(&norm, params.len(), module);
        self.anon.insert(norm, d);
        (d, params)
    }

    /// A fresh anonymous object def with fields `norm` (over `n` type params).
    fn new_anon_def(&mut self, norm: &[(String, TyId)], n: usize, module: usize) -> DefId {
        let name = {
            let parts: Vec<String> = norm
                .iter()
                .map(|(n, t)| format!("{n}: {}", self.display(*t)))
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
            .map(|(n, t)| FieldInfo {
                name: n.clone(),
                ty: *t,
                span: Span::DUMMY,
                readonly: false,
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
