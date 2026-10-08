//! Anonymous object types (`{ a: 1 }` literals, `{ kind: "circle"; readonly r: f64 }` written
//! types): one synthesized `AdtKind::Anon` def per shape (field names, declared types, `readonly`
//! and optional flags, in order), generic over the type parameters its fields mention (renumbered
//! by first occurrence). Shapes that differ only in `readonly` convert to each other
//! ([`Ctx::same_layout`]). An optional field (`a?: T`) and a `a: T | null` one hold the same
//! values, but are different shapes, as in TypeScript: `JSON.stringify` leaves out the first
//! while it is absent (JavaScript's missing property) and writes the second.
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

/// One field of an anonymous object type's shape.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ShapeField {
    pub name: String,
    /// The declared type, without the `null` a `?` adds (`FieldInfo::declared`).
    pub ty: TyId,
    pub readonly: bool,
    /// Declared `name?: T`: may be absent, and reads as `T | null`.
    pub optional: bool,
}

impl ShapeField {
    /// A field neither readonly nor optional.
    pub fn plain(name: &str, ty: TyId) -> ShapeField {
        ShapeField {
            name: name.to_string(),
            ty,
            readonly: false,
            optional: false,
        }
    }
}

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
        // Instances of object types that reach themselves are left as they are
        // (`reaches_itself`), so this only guards against an ICE elsewhere.
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
            } else if let Some(r) = self.canon_union(d, &args, depth) {
                out = r;
            }
        }
        self.canon_memo.insert(t, out);
        out
    }

    /// The canonical type of union def `d` applied to (canonical) `args`: the union of the
    /// substituted members, as if written directly (`A | B` at `A = B = string` is `string`, at
    /// `[i64, string]` the written `string | i64`). Only when every member is a plain type: a
    /// member that is itself a union or nullable would flatten into its parts, and lowering maps
    /// one generic variant onto one canonical variant (velt_vir `Cx::canon`, `union_variant`).
    fn canon_union(&mut self, d: DefId, args: &[TyId], depth: u32) -> Option<TyId> {
        if args.is_empty() || args.iter().any(|a| self.ty.has_error(*a)) {
            return None;
        }
        let t = self.ty.intern(TyKind::Adt(d, args.to_vec()));
        self.union_def(t)?;
        let members: Vec<TyId> = self
            .union_members(t)?
            .into_iter()
            .map(|m| self.canon_depth(m, depth + 1))
            .collect();
        let plain = |cx: &Self, m: TyId| {
            !matches!(
                cx.ty.kind(m),
                TyKind::Option(_) | TyKind::Unit | TyKind::Never | TyKind::Error
            ) && cx.union_def(m).is_none()
        };
        if !members.iter().all(|m| plain(self, *m)) {
            return None;
        }
        let u = self.union_of(&members, false, velt_common::Span::DUMMY);
        (u != t).then_some(u)
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
        if self.reaches_itself(d) {
            return None;
        }
        let templ: Vec<ShapeField> = a.fields.iter().map(shape_field).collect();
        // The flags are part of the shape: `{ readonly v: T0 }` at `string` is
        // `{ readonly v: string }`, not the writable `{ v: string }`.
        let fields: Vec<ShapeField> = templ
            .into_iter()
            .map(|f| {
                let ty = self.ty.subst(f.ty, args);
                let ty = self.canon_depth(ty, depth + 1);
                ShapeField { ty, ..f }
            })
            .collect();
        let (d2, args2) = self.anon_def_with(&fields, module);
        if d2 == d && args2 == args {
            return None;
        }
        Some(self.ty.intern(TyKind::Adt(d2, args2)))
    }

    /// Does anonymous def `d` reach itself through the fields of object types
    /// (`interface List<T> { tail?: List<T> }`, #376)? Such an instance stays as it is:
    /// canonicalizing it would canonicalize its own fields without end, and no written object
    /// type spells its shape without naming it.
    fn reaches_itself(&self, d: DefId) -> bool {
        let object = |x: DefId| {
            self.adt(x)
                .is_some_and(|a| a.kind == AdtKind::Anon || self.field_only_of.contains_key(&x))
        };
        let mut seen = std::collections::HashSet::new();
        let mut stack = self.anon_field_tys(d);
        while let Some(t) = stack.pop() {
            if !seen.insert(t) {
                continue;
            }
            let k = self.ty.kind(t);
            if let TyKind::Adt(x, _) = k {
                if *x == d {
                    return true;
                }
                if object(*x) {
                    stack.extend(self.anon_field_tys(*x));
                }
            }
            stack.extend(crate::types::children(k));
        }
        false
    }

    /// The anonymous object type with these fields (none readonly or optional).
    pub fn anon_type(&mut self, fields: &[(String, TyId)], module: usize) -> TyId {
        let (d, args) = self.anon_def(fields, module);
        self.ty.intern(TyKind::Adt(d, args))
    }

    /// The anonymous object type with these fields (declared types and flags).
    pub fn anon_type_with(&mut self, fields: &[ShapeField], module: usize) -> TyId {
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
        // Erasure turns a generic field-only interface's instance (`Pair<string, number>`) into
        // its generic anonymous twin applied to the arguments; canonical, that is the object type
        // it spells out.
        let ea = crate::readonly::erase_ty(&mut self.ty, &twins, &mut cache, a);
        let eb = crate::readonly::erase_ty(&mut self.ty, &twins, &mut cache, b);
        self.canon(ea) == self.canon(eb)
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

    /// The anonymous object def of this shape (no field readonly or optional) and its type
    /// arguments.
    pub fn anon_def(&mut self, fields: &[(String, TyId)], module: usize) -> (DefId, Vec<TyId>) {
        let fields: Vec<ShapeField> = fields
            .iter()
            .map(|(n, t)| ShapeField::plain(n, *t))
            .collect();
        self.anon_def_with(&fields, module)
    }

    /// The anonymous object def of this shape (declared types and flags) and its type
    /// arguments.
    pub fn anon_def_with(&mut self, fields: &[ShapeField], module: usize) -> (DefId, Vec<TyId>) {
        let mut params: Vec<u32> = vec![];
        for f in fields {
            crate::types::collect_params(&self.ty, f.ty, &mut params);
        }
        let new_tys: HashMap<u32, TyId> = params
            .iter()
            .enumerate()
            .map(|(i, p)| (*p, self.ty.param(i as u32)))
            .collect();
        let norm: Vec<ShapeField> = fields
            .iter()
            .map(|f| {
                let ty = self.ty.map(f.ty, &mut |k| match k {
                    TyKind::Param(i) => new_tys.get(i).copied(),
                    _ => None,
                });
                ShapeField { ty, ..f.clone() }
            })
            .collect();
        let params: Vec<TyId> = params.iter().map(|p| self.ty.param(*p)).collect();
        if let Some(&d) = self.anon.get(&norm) {
            return (d, params);
        }
        let d = self.new_anon_def(&norm, params.len(), module);
        self.anon.insert(norm.clone(), d);
        if norm.iter().any(|f| f.readonly) {
            let plain: Vec<ShapeField> = norm
                .iter()
                .map(|f| ShapeField {
                    readonly: false,
                    ..f.clone()
                })
                .collect();
            let (twin, _) = self.anon_def_with(&plain, module);
            self.readonly_twins.insert(d, (twin, None));
        }
        (d, params)
    }

    /// A fresh anonymous object def with fields `norm` (over `n` type params).
    fn new_anon_def(&mut self, norm: &[ShapeField], n: usize, module: usize) -> DefId {
        let name = {
            let parts: Vec<String> = norm
                .iter()
                .map(|f| {
                    let readonly = if f.readonly { "readonly " } else { "" };
                    let q = if f.optional { "?" } else { "" };
                    format!("{readonly}{}{q}: {}", f.name, self.display(f.ty))
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
            .map(|f| {
                // An optional field reads as `T | null` and may be left out, like a class's
                // (`body::driver`): its default is `null`, which lowering's JSON and printing
                // glue reads as "absent".
                let ty = if f.optional {
                    self.ty.option(f.ty)
                } else {
                    f.ty
                };
                let default = f.optional.then_some(crate::hir::Expr {
                    kind: crate::hir::ExprKind::Lit(crate::hir::Lit::Null),
                    ty,
                    span: Span::DUMMY,
                });
                FieldInfo {
                    name: f.name.clone(),
                    ty,
                    declared: f.ty,
                    span: Span::DUMMY,
                    readonly: f.readonly,
                    optional: f.optional,
                    has_default: f.optional,
                    default,
                    default_throws: vec![],
                    private_to: None,
                    inferred_int: false,
                }
            })
            .collect();
        self.adt_mut(d).fields = fields;
        d
    }
}

/// `hir::Program::anon_shapes`: every concrete anonymous def lowering sees, by its (erased)
/// fields. Defs replaced by a twin before lowering (`crate::readonly`) are left out: lowering
/// never sees them, and a shape must not resolve to one of them.
pub(crate) fn concrete_shapes(cx: &Ctx) -> HashMap<Vec<(String, TyId, bool)>, DefId> {
    let mut out = HashMap::new();
    for (i, d) in cx.defs.iter().enumerate() {
        let id = DefId(i as u32);
        if let Some(crate::hir::Def::Adt(a)) = d {
            if a.kind == AdtKind::Anon && a.generics == 0 && !cx.readonly_twins.contains_key(&id) {
                let key = a
                    .fields
                    .iter()
                    .map(|f| (f.name.clone(), f.ty, f.optional))
                    .collect();
                out.entry(key).or_insert(id);
            }
        }
    }
    out
}

/// `hir::Program::union_shapes`: every concrete union def by its member list (sorted by type id,
/// as `Ctx::union_of` orders concrete members).
pub(crate) fn concrete_unions(cx: &Ctx) -> HashMap<Vec<TyId>, DefId> {
    let mut out = HashMap::new();
    for (key, d) in &cx.unions {
        if let Some(crate::hir::Def::Enum(e)) = &cx.defs[d.0 as usize] {
            if e.generics == 0 {
                out.entry(key.clone()).or_insert(*d);
            }
        }
    }
    out
}

/// Field `f` as an anonymous object type's shape holds it.
pub(crate) fn shape_field(f: &FieldInfo) -> ShapeField {
    ShapeField {
        name: f.name.clone(),
        ty: f.declared,
        readonly: f.readonly,
        optional: f.optional,
    }
}

/// Whether field `f` of a type of kind `kind` keeps "absent" apart from a present `null`
/// (`hir::FieldDef::presence`): an optional field whose declared type is nullable
/// (`a?: T | null`), in an object type or struct (P2b, deferred-types.md).
pub(crate) fn has_presence(ty: &crate::types::Types, kind: AdtKind, f: &FieldInfo) -> bool {
    f.optional && kind != AdtKind::Class && ty.opt_payload(f.declared).is_some()
}
